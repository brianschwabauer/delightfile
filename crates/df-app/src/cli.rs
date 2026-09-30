//! The command line: `delightfile [path]`, `--reveal`, `--cwd-file=<path>`,
//! `--chooser-file=<path>` and the three `--chooser-*` switches that say what
//! kind of dialog it is standing in for (PLAN §3), `--chooser-request=<path>`
//! for the whole of a dialog, and `--portal`.
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
//!
//! Three switches are all a wrapper *can* pass. `--chooser-request` is the
//! rest of the dialog — its title, the caller's button label, the suggested
//! name, where to start, the file-type filters — as a small TOML file that
//! `delightfile --portal` (see [`crate::platform::portal`]) writes for each dialog it
//! is asked for and removes once the window has answered.
//!
//! `--reveal` is the file manager's side of the same service: "Show in
//! folder" reaches `--portal` as `org.freedesktop.FileManager1.ShowItems`,
//! and each window it opens for that is `delightfile --reveal <path>`. A bare
//! path cannot say it, because a bare path to a folder opens the folder, and
//! showing a folder means showing it among its siblings.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    /// Where to start. `None` means the process's current directory.
    pub start: Option<PathBuf>,
    /// `--reveal`: `start` is to be shown, not opened — the window opens the
    /// folder it is in with the cursor on it, whether it is a file or a
    /// folder (see `reveal_directory` in `app.rs`).
    pub reveal: bool,
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
    /// `--portal`: no window, just the file-chooser backend and
    /// `org.freedesktop.FileManager1` on the session bus. Only produced where
    /// there is one ([`crate::platform::HAS_PORTAL`]).
    Portal,
    /// Print this and exit 0 — `--help`, `--version`.
    Print(String),
    /// Print this to stderr and exit 2.
    Fail(String),
}

/// The `--help` text. Written out rather than generated: it is the one place a
/// person reads what the flags are, so it is prose, not a table dump.
///
/// In pieces, because the first line and three entries differ by platform
/// ([`crate::platform::cli`]): Linux's says it is for Wayland, `--portal`
/// exists only where there is a desktop portal to serve, and the two flags
/// whose Linux lines say the portal uses them say nothing of it anywhere
/// else. The Select button's chord is written the platform's way:
/// `Ctrl+Enter`, or `⌘Enter` on a Mac.
pub fn usage() -> String {
    use crate::platform::cli::{EXTRA_USAGE, REQUEST_USAGE, REVEAL_USAGE, TITLE_USAGE};
    let chooser = USAGE_CHOOSER.replace("{choose}", crate::keys::written("ctrl+enter"));
    [
        TITLE_USAGE,
        USAGE_HEAD,
        REVEAL_USAGE,
        &chooser,
        REQUEST_USAGE,
        EXTRA_USAGE,
        USAGE_TAIL,
    ]
    .concat()
}

/// `--help` after its first line, down to `--reveal`.
const USAGE_HEAD: &str = "
usage: delightfile [path] [options]

  path                   the directory to open (default: the current one);
                         a file opens its directory with the cursor on it
";

/// `--help` from after `--reveal` to `--chooser-request`. `{choose}` is the
/// Select button's chord, which [`usage`] writes in.
const USAGE_CHOOSER: &str =
    "  --cwd-file=<path>      write the final directory here when quitting with `q`
                         (`Q` quits without writing it)
  --chooser-file=<path>  pick rather than open: `Enter` or the Select button
                         (`{choose}`) writes the picked paths here, one per
                         line, and quits. Quitting any other way writes
                         nothing, which is a cancel.
  --chooser-multiple     the dialog takes several files (default: one)
  --chooser-directory    the dialog wants a folder: Choose folder picks the
                         selected folders, or the one you are in
  --chooser-save         the dialog is a save: pick a file to replace, or
                         Save to type a new name
                         (the three above need --chooser-file)
";

/// `--help` after the platform's own flags.
const USAGE_TAIL: &str = "  -h, --help             show this
  -V, --version          show the version
";

/// Parse everything after the program name.
///
/// As `OsString`s, the way `std::env::args_os` hands them over: a path is
/// bytes, and a file whose name is not UTF-8 is still a file somebody can ask
/// to be shown. Only an option's *name* has to be text; what it is given, and
/// every positional path, is kept byte for byte.
pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Outcome {
    let mut out = Args::default();
    let mut rest_are_paths = false;
    // The switches are collected on their own and folded into a `Chooser`
    // at the end, because they may come before `--chooser-file` as well as
    // after it — the order a person types flags in is not a meaning.
    let mut chooser_file: Option<PathBuf> = None;
    let mut chooser_request: Option<PathBuf> = None;
    let (mut multiple, mut directory, mut save) = (false, false, false);
    let (mut portal, mut count) = (false, 0);
    let mut reveal = false;
    for arg in args {
        count += 1;
        if rest_are_paths || !is_option(&arg) {
            if out.start.is_some() {
                return Outcome::Fail(format!(
                    "only one path can be opened (got `{}` as well)",
                    arg.to_string_lossy()
                ));
            }
            out.start = Some(PathBuf::from(arg));
            continue;
        }
        // An option that is not text is none of the switches, and falls
        // through to the flags, whose values need not be.
        match arg.to_str().unwrap_or_default() {
            // The POSIX end-of-options marker, so a directory literally called
            // `--help` is still openable.
            "--" => rest_are_paths = true,
            "-h" | "--help" => return Outcome::Print(usage()),
            "-V" | "--version" => {
                return Outcome::Print(format!("delightfile {}\n", env!("CARGO_PKG_VERSION")))
            }
            "--chooser-multiple" => multiple = true,
            "--chooser-directory" => directory = true,
            "--chooser-save" => save = true,
            // Only where there is a portal to serve; anywhere else it is an
            // option like any other this program does not have.
            "--portal" if crate::platform::HAS_PORTAL => portal = true,
            "--reveal" => reveal = true,
            _ => {
                let found = ["--cwd-file", "--chooser-file", "--chooser-request"]
                    .into_iter()
                    .find_map(|name| match flag(&arg, name) {
                        Flag::Absent => None,
                        found => Some((name, found)),
                    });
                let Some((name, found)) = found else {
                    return Outcome::Fail(format!(
                        "unknown option `{}`\n\n{}",
                        arg.to_string_lossy(),
                        usage()
                    ));
                };
                let Flag::Value(path) = found else {
                    return Outcome::Fail(format!("{name} needs a path"));
                };
                let path = Some(PathBuf::from(path));
                match name {
                    "--cwd-file" => out.cwd_file = path,
                    "--chooser-file" => chooser_file = path,
                    _ => chooser_request = path,
                }
            }
        }
    }
    // The backend has no window to aim, so anything beside it is a mistake
    // in whatever started it, said rather than ignored.
    if portal {
        return if count == 1 {
            Outcome::Portal
        } else {
            Outcome::Fail("--portal takes no other arguments".to_string())
        };
    }
    // Revealing is the file manager pointing at something, and a dialog is a
    // window answering somebody else's question from where that question
    // said to start; a window cannot be both. And with no path there is
    // nothing to point at — refused, like a chooser switch with nowhere to
    // answer, rather than quietly opening the current directory.
    if reveal {
        if chooser_file.is_some() || chooser_request.is_some() || multiple || directory || save {
            return Outcome::Fail("--reveal cannot be used with the --chooser-* options".into());
        }
        if out.start.is_none() {
            return Outcome::Fail("--reveal needs a path to show".into());
        }
        out.reveal = true;
    }
    match (chooser_file, chooser_request) {
        // The request file says everything the switches could, and more; a
        // switch given beside it is outranked rather than merged, because
        // "a save that is also a folder dialog" is not a dialog.
        (Some(out_path), Some(request)) => match read_request(&request, out_path) {
            Ok((chooser, start)) => {
                out.chooser = Some(chooser);
                // A path on the command line is a person overriding the
                // request; it wins, as it would over any default.
                out.start = out.start.or(start);
            }
            Err(message) => return Outcome::Fail(message),
        },
        (None, Some(_)) => {
            return Outcome::Fail(
                "--chooser-request: only meaningful with --chooser-file".to_string(),
            )
        }
        (Some(out_path), None) => {
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
        (None, None) if multiple || directory || save => {
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
        (None, None) => {}
    }
    Outcome::Run(out)
}

/// Whether `arg` is spelled as an option: it starts with `-`.
///
/// Read off the argument's bytes ([`df_core::platform::os::as_bytes`]), the
/// kernel's own on Linux and macOS. Only on Windows can an argument have none
/// — a name that is not valid Unicode — and that one is a path, since an
/// option's name has to be text.
fn is_option(arg: &OsStr) -> bool {
    df_core::platform::os::as_bytes(arg).is_ok_and(|bytes| bytes.starts_with(b"-"))
}

/// What `--name=value` had to say about itself.
enum Flag {
    /// This is not that flag.
    Absent,
    /// This is that flag, and it was given nothing.
    Empty,
    Value(OsString),
}

/// Read `--name=value` out of one argument.
///
/// Three flags take a path and all reject an empty one, so the reading and
/// the rejecting are one function rather than the same three lines thrice.
///
/// The value is a path, so it is read as bytes: only the name is text. The
/// bytes are the platform's ([`df_core::platform::os`]), exact on Linux and
/// macOS; on Windows an argument or a value it cannot spell is not the flag.
fn flag(arg: &OsStr, name: &str) -> Flag {
    let Ok(bytes) = df_core::platform::os::as_bytes(arg) else {
        return Flag::Absent;
    };
    match bytes
        .strip_prefix(name.as_bytes())
        .and_then(|rest| rest.strip_prefix(b"="))
    {
        Some(value) if !value.is_empty() => match df_core::platform::os::from_bytes(value) {
            Ok(value) => Flag::Value(value),
            Err(_) => Flag::Absent,
        },
        Some(_) => Flag::Empty,
        None => Flag::Absent,
    }
}

/// Read a `--chooser-request` file into the chooser it describes, answering
/// to `out`, and the path the window should start at.
pub fn read_request(path: &Path, out: PathBuf) -> Result<(Chooser, Option<PathBuf>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_request(&text, path, out)
}

/// The request file's text, understood — the format `crate::platform::portal` writes:
///
/// ```toml
/// kind = "open"            # or "save", or "save-files" (a folder to save into)
/// title = "Open File"
/// accept = "Upload"
/// multiple = true
/// directory = false
/// folder = "/home/brian/Pictures"
/// name = "untitled.png"    # a save's suggested name
/// file = "/home/brian/x.png"  # the file a save is saving over
/// current_filter = "Images"
///
/// [[filter]]
/// name = "Images"
/// glob = ["*.png", "*.jpg"]
/// mime = ["image/*"]
/// ```
///
/// Only `kind` is required. Unlike the config files, which apply every line
/// they can, a line this cannot read fails the whole request: the file is
/// written by a program, so a bad line is a bug, and a dialog opened on half
/// its description could answer the wrong question.
pub fn parse_request(
    text: &str,
    path: &Path,
    out: PathBuf,
) -> Result<(Chooser, Option<PathBuf>), String> {
    use df_core::toml::{Table, Value};

    let doc = df_core::toml::parse(text, path);
    if let Some(warning) = doc.warnings.first() {
        return Err(warning.to_string());
    }
    let empty = Table::default();
    let root = doc.root().unwrap_or(&empty);
    let wrong = |key: &str, value: &Value, wanted: &str| {
        format!(
            "{}: `{key}` should be {wanted}, not {}",
            path.display(),
            value.type_name()
        )
    };
    let string = |table: &Table, key: &str| match table.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| wrong(key, value, "a string")),
    };
    let strings = |table: &Table, key: &str| match table.get(key) {
        None => Ok(Vec::new()),
        Some(value) => value
            .as_str_array()
            .map(|items| items.into_iter().map(str::to_string).collect())
            .ok_or_else(|| wrong(key, value, "a list of strings")),
    };
    let switch = |key: &str| match root.get(key) {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| wrong(key, value, "true or false")),
    };

    let mut chooser = Chooser::new(out);
    match string(root, "kind")?.as_deref() {
        Some("open") => {
            chooser.multiple = switch("multiple")?;
            chooser.directory = switch("directory")?;
        }
        Some("save") => chooser.save = true,
        // A folder to save several files into: to the window, a folder dialog.
        Some("save-files") => chooser.directory = true,
        Some(other) => return Err(format!("{}: unknown kind `{other}`", path.display())),
        None => return Err(format!("{}: no `kind`", path.display())),
    }
    chooser.title = string(root, "title")?;
    chooser.accept = string(root, "accept")?;
    chooser.name = string(root, "name")?;
    for table in doc.tables_named("filter") {
        chooser.filters.push(TypeFilter {
            name: string(table, "name")?.unwrap_or_default(),
            globs: strings(table, "glob")?,
            mimes: strings(table, "mime")?,
        });
    }
    chooser.current_filter = string(root, "current_filter")?
        .and_then(|name| chooser.filters.iter().position(|f| f.name == name))
        .unwrap_or(0);
    let folder = string(root, "folder")?.map(PathBuf::from);
    let file = string(root, "file")?.map(PathBuf::from);
    Ok((chooser, request_start(folder, file)))
}

/// Where a request's window starts, as a positional path would say it.
///
/// A file that exists wins when it is in the folder (or there is no folder):
/// a path to a file already means "its folder, cursor on it" (see
/// `start_directory` in `app.rs`), which is how a save over an existing file
/// should open. A file that does not exist yet cannot be aimed at, so the
/// window starts where it would be.
fn request_start(folder: Option<PathBuf>, file: Option<PathBuf>) -> Option<PathBuf> {
    match (folder, file) {
        (folder, Some(file))
            if file.exists()
                && folder
                    .as_deref()
                    .is_none_or(|folder| file.parent() == Some(folder)) =>
        {
            Some(file)
        }
        (Some(folder), _) => Some(folder),
        (None, Some(file)) => file.parent().map(Path::to_path_buf),
        (None, None) => None,
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
        parse(args.iter().map(OsString::from))
    }

    /// A name that is not UTF-8 is a path like any other: positional, after
    /// `--reveal`, and as what a flag is given. An *option* that is not text
    /// is an unknown option, said with the bytes it could not read replaced.
    #[test]
    #[cfg(unix)] // a name that is not UTF-8 is a Unix name
    fn a_path_that_is_not_utf8_is_kept_byte_for_byte() {
        use std::os::unix::ffi::OsStringExt;
        let bytes = |bytes: &[u8]| OsString::from_vec(bytes.to_vec());
        let odd = bytes(b"/tmp/caf\xe9.txt");

        assert_eq!(
            parse([odd.clone()]),
            Outcome::Run(Args {
                start: Some(PathBuf::from(&odd)),
                ..Args::default()
            })
        );
        assert_eq!(
            parse([
                OsString::from("--reveal"),
                OsString::from("--"),
                odd.clone()
            ]),
            Outcome::Run(Args {
                start: Some(PathBuf::from(&odd)),
                reveal: true,
                ..Args::default()
            })
        );
        assert_eq!(
            parse([bytes(b"--cwd-file=/tmp/caf\xe9.txt")]),
            Outcome::Run(Args {
                cwd_file: Some(PathBuf::from(&odd)),
                ..Args::default()
            })
        );
        match parse([bytes(b"--caf\xe9")]) {
            Outcome::Fail(message) => {
                assert!(
                    message.contains("unknown option `--caf\u{FFFD}`"),
                    "{message}"
                )
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn a_bare_path_is_the_starting_directory() {
        assert_eq!(
            parse_str(&["/tmp"]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/tmp")),
                reveal: false,
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
                reveal: false,
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
                reveal: false,
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
                reveal: false,
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
                reveal: false,
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
                reveal: false,
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
                reveal: false,
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

    /// `--portal` is the backend and nothing else — where there is one. Where
    /// there is not, it is an unknown option.
    #[test]
    fn portal_stands_alone() {
        if crate::platform::HAS_PORTAL {
            assert_eq!(parse_str(&["--portal"]), Outcome::Portal);
        } else {
            match parse_str(&["--portal"]) {
                Outcome::Fail(message) => {
                    assert!(
                        message.starts_with("unknown option `--portal`"),
                        "{message}"
                    )
                }
                other => panic!("--portal parsed as {other:?}"),
            }
        }
        assert!(matches!(parse_str(&["--portal", "/tmp"]), Outcome::Fail(_)));
        assert!(matches!(
            parse_str(&["--chooser-file=/tmp/out", "--portal"]),
            Outcome::Fail(_)
        ));
    }

    /// `--help` names the portal exactly where there is one: its own entry,
    /// and the two flags whose lines say the portal uses them.
    #[test]
    fn the_help_names_the_portal_only_where_there_is_one() {
        let Outcome::Print(help) = parse_str(&["--help"]) else {
            panic!("--help did not print");
        };
        assert_eq!(
            help.contains("portal"),
            crate::platform::HAS_PORTAL,
            "{help}"
        );
        for flag in ["--reveal", "--cwd-file", "--chooser-request", "--help"] {
            assert!(help.contains(flag), "{flag} is missing from:\n{help}");
        }
        assert!(help.ends_with("show the version\n"));
        // Every entry keeps its indent across the seams between the pieces.
        assert!(
            help.lines().all(|line| !line.starts_with('-')),
            "an entry lost its indent:\n{help}"
        );
    }

    /// `--help` as each platform prints it, whole: Linux's exactly as it
    /// always was, macOS's and Windows' with no Wayland in the first line
    /// and nothing of the portal (D5.9).
    #[test]
    fn the_help_is_the_platforms_own() {
        const LINUX: &str = r#"delightfile — a keyboard-first file manager for Wayland

usage: delightfile [path] [options]

  path                   the directory to open (default: the current one);
                         a file opens its directory with the cursor on it
  --reveal               show the path rather than open it: its folder opens
                         with the cursor on it, a folder included (what
                         "Show in folder" asks --portal for; needs a path,
                         and cannot be a --chooser-* dialog)
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
  --chooser-request=<path>
                         read the whole dialog from this file instead: its
                         kind, title, button label, suggested name, starting
                         folder and file-type filters (written by --portal;
                         needs --chooser-file, and outranks the switches)
  --portal               serve the xdg-desktop-portal file chooser, and
                         org.freedesktop.FileManager1 ("Show in folder"), on
                         the session bus; D-Bus starts this, not a person
  -h, --help             show this
  -V, --version          show the version
"#;
        const ELSEWHERE: &str = r#"delightfile — a keyboard-first file manager

usage: delightfile [path] [options]

  path                   the directory to open (default: the current one);
                         a file opens its directory with the cursor on it
  --reveal               show the path rather than open it: its folder opens
                         with the cursor on it, a folder included (needs a
                         path, and cannot be a --chooser-* dialog)
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
  --chooser-request=<path>
                         read the whole dialog from this file instead: its
                         kind, title, button label, suggested name, starting
                         folder and file-type filters (needs --chooser-file,
                         and outranks the switches)
  -h, --help             show this
  -V, --version          show the version
"#;
        let expected = if cfg!(target_os = "linux") {
            LINUX.to_string()
        } else {
            // The Select chord is the one line written the platform's way.
            ELSEWHERE.replace("Ctrl+Enter", crate::keys::written("ctrl+enter"))
        };
        assert_eq!(usage(), expected);
    }

    /// `--reveal` marks the path as one to show; it needs a path, and it is
    /// never a dialog.
    #[test]
    fn reveal_shows_a_path_and_is_never_a_dialog() {
        let revealed = Outcome::Run(Args {
            start: Some(PathBuf::from("/home/brian/Downloads")),
            reveal: true,
            cwd_file: None,
            chooser: None,
        });
        assert_eq!(parse_str(&["--reveal", "/home/brian/Downloads"]), revealed);
        // Anywhere on the line, and before the end of the options — the
        // shape `--portal` starts its windows with.
        assert_eq!(parse_str(&["/home/brian/Downloads", "--reveal"]), revealed);
        assert_eq!(
            parse_str(&["--reveal", "--", "/home/brian/Downloads"]),
            revealed
        );
        // After `--` it is a name like any other.
        assert_eq!(
            parse_str(&["--", "--reveal"]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("--reveal")),
                ..Args::default()
            })
        );
        // Beside the cwd-file it is still a plain window.
        let Outcome::Run(args) = parse_str(&["--cwd-file=/tmp/cwd", "--reveal", "/tmp"]) else {
            panic!("did not parse");
        };
        assert!(args.reveal);
        assert_eq!(args.cwd_file, Some(PathBuf::from("/tmp/cwd")));

        match parse_str(&["--reveal"]) {
            Outcome::Fail(message) => assert!(message.contains("--reveal"), "{message}"),
            other => panic!("--reveal alone parsed as {other:?}"),
        }
        for chooser in [
            "--chooser-file=/tmp/out",
            "--chooser-request=/tmp/request.toml",
            "--chooser-multiple",
            "--chooser-directory",
            "--chooser-save",
        ] {
            match parse_str(&["--reveal", chooser, "/tmp/x"]) {
                Outcome::Fail(message) => assert!(message.contains("--reveal"), "{message}"),
                other => panic!("--reveal with {chooser} parsed as {other:?}"),
            }
        }
        assert!(matches!(
            parse_str(&["--portal", "--reveal"]),
            Outcome::Fail(_)
        ));
        assert!(matches!(
            parse_str(&["--reveal=1", "/tmp"]),
            Outcome::Fail(_)
        ));
    }

    /// A scratch request file with `text` in it.
    fn request_file(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("df-request-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let path = dir.join("request.toml");
        std::fs::write(&path, text).expect("written");
        path
    }

    /// The request file fills in the whole chooser, and its folder is where
    /// the window starts — unless a path on the command line says otherwise.
    #[test]
    fn a_request_file_describes_the_whole_dialog() {
        let request = request_file(
            "whole",
            r#"
kind = "open"
title = "Upload to Drive"
accept = "Upload"
multiple = true
folder = "/srv/photos"
current_filter = "Text"

[[filter]]
name = "Images"
glob = ["*.png"]
mime = ["image/*"]

[[filter]]
name = "Text"
mime = ["text/plain"]
"#,
        );
        let chooser_request = format!("--chooser-request={}", request.display());
        let Outcome::Run(args) = parse_str(&["--chooser-file=/tmp/out", &chooser_request]) else {
            panic!("did not parse");
        };
        assert_eq!(args.start, Some(PathBuf::from("/srv/photos")));
        let chooser = args.chooser.expect("a chooser");
        assert_eq!(chooser.mode(), PickMode::Files);
        assert_eq!(chooser.title.as_deref(), Some("Upload to Drive"));
        assert_eq!(chooser.accept.as_deref(), Some("Upload"));
        assert_eq!(chooser.filters.len(), 2);
        assert_eq!(chooser.filters[1].globs, Vec::<String>::new());
        assert_eq!(chooser.current_filter, 1);

        // A positional path wins over the request's folder, and the switches
        // are outranked by its kind.
        let Outcome::Run(args) = parse_str(&[
            "--chooser-save",
            &chooser_request,
            "--chooser-file=/tmp/out",
            "/home/brian",
        ]) else {
            panic!("did not parse");
        };
        assert_eq!(args.start, Some(PathBuf::from("/home/brian")));
        assert_eq!(args.chooser.map(|c| c.mode()), Some(PickMode::Files));
        let _ = std::fs::remove_dir_all(request.parent().expect("a dir"));
    }

    /// A request with nowhere to answer, one that cannot be read, and one that
    /// does not say what kind of dialog it is are refused, not guessed at.
    #[test]
    fn a_bad_request_file_is_refused() {
        let good = request_file("good", "kind = \"save\"\n");
        let good_flag = format!("--chooser-request={}", good.display());
        assert!(matches!(parse_str(&[&good_flag]), Outcome::Fail(_)));
        assert!(matches!(
            parse_str(&["--chooser-file=/tmp/out", "--chooser-request="]),
            Outcome::Fail(_)
        ));
        assert!(matches!(
            parse_str(&[
                "--chooser-file=/tmp/out",
                "--chooser-request=/nonexistent/request.toml"
            ]),
            Outcome::Fail(_)
        ));
        for (name, text) in [
            ("nokind", "title = \"x\"\n"),
            ("badkind", "kind = \"delete\"\n"),
            ("badtype", "kind = \"open\"\nmultiple = \"yes\"\n"),
            ("badline", "kind = \"open\"\nthis is not toml\n"),
            (
                "badglob",
                "kind = \"open\"\n[[filter]]\nname = \"x\"\nglob = [1]\n",
            ),
        ] {
            let path = request_file(name, text);
            let flag = format!("--chooser-request={}", path.display());
            match parse_str(&["--chooser-file=/tmp/out", &flag]) {
                Outcome::Fail(_) => {}
                other => panic!("{name}: {other:?}"),
            }
            let _ = std::fs::remove_dir_all(path.parent().expect("a dir"));
        }
        let Outcome::Run(args) = parse_str(&["--chooser-file=/tmp/out", &good_flag]) else {
            panic!("a bare save request parses");
        };
        assert_eq!(args.chooser.map(|c| c.mode()), Some(PickMode::Save));
        assert_eq!(args.start, None);
        let _ = std::fs::remove_dir_all(good.parent().expect("a dir"));
    }

    /// A directory really can be called `--help`.
    #[test]
    fn double_dash_ends_the_options() {
        assert_eq!(
            parse_str(&["--", "--help"]),
            Outcome::Run(Args {
                start: Some(PathBuf::from("--help")),
                reveal: false,
                cwd_file: None,
                chooser: None,
            })
        );
    }
}
