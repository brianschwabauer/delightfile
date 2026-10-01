//! What a fresh install ships on Windows
//! (`plans/other-platforms/05-defaults-and-config.md` §2.2, §2.3, §6).
//!
//! Openers are argv lines here, not shell strings: df-app's Windows opener
//! splits a command on whitespace, groups double quotes, and puts the paths
//! in for `$1`, `$@` and `$dir`, nothing else (W4.3). What a fresh machine
//! has is the system's own "open" (`builtin:shell-open`), Notepad and `cmd`;
//! VS Code, Windows Terminal, Zed and Chrome are used where they are
//! installed.
//!
//! Some rows have two programs, the better one first — `code` then `notepad`
//! to edit, `wt` then `cmd` for a terminal. [`openers`] picks, per row, the
//! first whose program is on `PATH` when it is asked (when [`crate::config::
//! Config::default`] builds the table, once a start), so the table the app
//! holds has one command per row and the config format is unchanged (D5.4). A
//! program installed while delightfile runs is seen at its next start.
//!
//! The rules are Linux's row for row, as on macOS: the system's default app
//! (`open`) where Linux has delightviewer, and the openers Windows does not
//! ship left out — delightviewer's editor, the wallpaper and AVIF helpers,
//! and `edit-image`, which is `open` here.
//!
//! **The bookmarks are a Windows user's places** (W4.40): the profile folder
//! as Home, the known folders where the shell says they are — a Documents
//! OneDrive took over is found in `OneDrive` — the system drive's root, and
//! `%APPDATA%`, where delightfile's own config lives. Each is written as
//! Windows writes it, `C:\Users\admin\Downloads`, and says so on the
//! which-key card. `g /` has no root to go to here, and opens the Places
//! card, whose Drives section is what a root would have listed.

use std::path::Path;

use crate::config::Bookmark;

use super::known::{self, Known};

/// Openers a rule can name: id, the commands to choose from (the first
/// whose program is on `PATH`, else the last), blocking, description.
pub const CANDIDATES: &[(&str, &[&str], bool, &str)] = &[
    ("open", &["builtin:shell-open"], false, "Open"),
    (
        "edit",
        &[r#"code --wait "$@""#, r#"notepad "$@""#],
        false,
        "Edit",
    ),
    ("zed", &[r#"zed "$@""#], false, "Open in Zed"),
    (
        "zed-workspace",
        &[r#"zed "$1""#],
        false,
        "Open folder in Zed",
    ),
    (
        "terminal-here",
        &[r#"wt -d "$1""#, r#"cmd /K cd /d "$1""#],
        false,
        "Terminal here",
    ),
    (
        "terminal-at",
        &[r#"wt -d "$dir""#, r#"cmd /K cd /d "$dir""#],
        false,
        "Terminal at file",
    ),
    (
        "open-in-chrome",
        &[r#"chrome "$@""#],
        false,
        "Open in Chrome",
    ),
    ("play", &["builtin:shell-open"], false, "Play"),
    (
        "bulk-rename",
        &[r#"code --wait "$@""#, r#"notepad "$@""#],
        true,
        "Bulk rename",
    ),
    ("extract", &["builtin:extract"], false, "Extract to folder"),
    (
        "extract-here",
        &["builtin:extract-here"],
        false,
        "Extract here",
    ),
    (
        "extract-merged",
        &["builtin:extract-merged"],
        false,
        "Extract all into one folder",
    ),
];

/// The openers a fresh install ships, one command per row: each row's first
/// candidate whose program is on `PATH` now, else its last.
pub fn openers() -> Vec<(&'static str, &'static str, bool, &'static str)> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let found = |program: &str| on_path(&path, program);
    CANDIDATES
        .iter()
        .map(|(id, commands, block, description)| {
            (*id, pick(commands, &found), *block, *description)
        })
        .collect()
}

/// The first of `commands` whose program `found` says is there, else the
/// last. A builtin needs no program.
fn pick(commands: &[&'static str], found: &dyn Fn(&str) -> bool) -> &'static str {
    commands
        .iter()
        .find(|command| command.starts_with("builtin:") || program(command).is_some_and(found))
        .or(commands.last())
        .copied()
        .unwrap_or_default()
}

/// The program a command runs: its first word.
fn program(command: &str) -> Option<&str> {
    command.split_whitespace().next()
}

/// Whether `program` is on `path` (`PATH`'s value) under one of the names
/// Windows would run it by (`platform::process::candidates`: `code.exe`,
/// `code`; `.cmd` for VS Code's shim through `PATHEXT`).
fn on_path(path: &std::ffi::OsStr, program: &str) -> bool {
    let mut names = crate::platform::process::candidates(program);
    names.push(format!("{program}.cmd"));
    std::env::split_paths(path).any(|dir| names.iter().any(|name| is_program(&dir.join(name))))
}

fn is_program(path: &Path) -> bool {
    crate::platform::process::is_executable(path)
}

/// Opener rules, matched top-down: Linux's
/// ([`crate::config::DEFAULT_RULES`]) row for row, with `open` — the
/// system's default app — where Linux has delightviewer, and the openers
/// Windows does not ship (delightviewer's editor, the wallpaper and AVIF
/// helpers, `edit-image`) left out. An `open` a row would then name twice is
/// named once.
pub const RULES: &[(&str, &str, &[&str])] = &[
    ("glob", "bulk-rename.txt", &["bulk-rename"]),
    ("glob", "*.{stl,obj,ply,3mf}", &["open", "terminal-at"]),
    ("glob", "*.{gcode,gco}", &["open", "edit", "terminal-at"]),
    ("glob", "*.{ttf,otf,ttc}", &["open", "terminal-at"]),
    (
        "glob",
        "*.{zip,tar,tgz,gz,bz2,xz,zst,7z,rar,cbz,cbr}",
        &["extract", "extract-here", "extract-merged", "open"],
    ),
    (
        "mime",
        "application/{zip,x-tar,gzip,x-bzip2,x-xz,zstd,x-7z-compressed,vnd.rar}",
        &["extract", "extract-here", "extract-merged", "open"],
    ),
    (
        "mime",
        "text/html",
        &["zed", "open-in-chrome", "edit", "open", "terminal-at"],
    ),
    ("mime", "text/*", &["zed", "edit", "open", "terminal-at"]),
    (
        "mime",
        "application/{json,ndjson,xml,javascript,x-shellscript,x-yaml,toml}",
        &["zed", "edit", "open", "terminal-at"],
    ),
    ("mime", "image/*", &["open", "terminal-at"]),
    ("mime", "video/*", &["open", "play", "terminal-at"]),
    ("mime", "audio/*", &["open", "terminal-at"]),
    ("mime", "application/pdf", &["open", "terminal-at"]),
    ("glob", "*/", &["open", "zed-workspace", "terminal-here"]),
    ("glob", "*", &["open", "terminal-at"]),
];

/// The `g` chord's bookmarks, in which-key order: Home, the six known
/// folders, the system drive, `%APPDATA%`. A folder this machine cannot
/// name is left out rather than shipped as a place that is not there.
pub fn bookmarks() -> Vec<Bookmark> {
    let mut rows = Vec::new();
    if let Some(home) = super::dirs::home() {
        rows.push(named("h", &home, "Home"));
    }
    for (key, which) in [
        ("d", Known::Downloads),
        ("D", Known::Desktop),
        ("o", Known::Documents),
        ("p", Known::Pictures),
        ("v", Known::Videos),
        ("m", Known::Music),
    ] {
        if let Some(path) = known::folder(which) {
            rows.push(named(key, &path, which.name()));
        }
    }
    let drive = system_drive(std::env::var("SystemDrive").ok().as_deref());
    rows.push(Bookmark::row(
        "c",
        &drive,
        &format!("Drive {}", drive.trim_end_matches('\\')),
    ));
    if let Some(appdata) = super::dirs::config_dir() {
        rows.push(named("a", &appdata, "AppData"));
    }
    rows
}

/// A bookmark to `path` that the Places card calls `name`, and the
/// which-key card names with where it is: `Home · C:\Users\admin`.
fn named(key: &str, path: &Path, name: &str) -> Bookmark {
    let path = crate::path::display(path);
    Bookmark {
        key: key.to_string(),
        description: format!("{name} · {path}"),
        path,
        name: Some(name.to_string()),
    }
}

/// The system drive's root, `C:\`, from `%SystemDrive%` (`C:`), which a
/// stripped environment may not have.
fn system_drive(variable: Option<&str>) -> String {
    let drive = variable
        .map(str::trim)
        .filter(|drive| drive.len() == 2 && drive.ends_with(':'))
        .unwrap_or("C:");
    format!("{drive}\\")
}

/// Rows the platform lays over the shipped keymap. `g /` is the Places
/// card, a Windows machine having no root for it to go to; Linux and macOS
/// leave `g /` unbound.
pub const KEYMAP_OVERRIDES: &[(&str, &str, &str, &str)] = &[(
    "files",
    "g /",
    "mount-manager",
    "Places: drives and network",
)];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Opener};
    use crate::keymap::{parse_sequence, Command, Context, Registry};

    /// A fresh install goes where a Windows user's places are: the keys,
    /// in which-key order, each a full path written with `\`, Home and
    /// AppData named for the Places card, every description saying where.
    #[test]
    fn default_bookmarks_are_a_windows_users_places() {
        let c = Config::default();
        let keys: Vec<&str> = c.goto.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, ["h", "d", "D", "o", "p", "v", "m", "c", "a"]);
        for b in &c.goto {
            assert!(Path::new(&b.path).is_absolute(), "{b:?}");
            assert!(!b.path.contains('/'), "{b:?}");
            assert!(!b.path.starts_with('~'), "{b:?}");
            assert!(b.description.contains(&b.path), "{b:?}");
        }
        let at = |key: &str| c.goto.iter().find(|b| b.key == key).expect(key);
        let home = super::super::dirs::home().expect("a profile");
        assert_eq!(Path::new(&at("h").path), home);
        assert_eq!(at("h").name.as_deref(), Some("Home"));
        assert_eq!(at("h").description, format!("Home · {}", home.display()));
        assert_eq!(at("a").name.as_deref(), Some("AppData"));
        assert_eq!(
            Path::new(&at("a").path),
            super::super::dirs::config_dir().expect("appdata")
        );
        assert!(at("c").path.ends_with(":\\"), "{:?}", at("c"));
        assert_eq!(
            Path::new(&at("d").path),
            known::folder(Known::Downloads).expect("Downloads")
        );
        assert!(c.goto.iter().all(|b| !b.path.contains("Work")));
    }

    /// `%SystemDrive%` is the drive `g c` goes to the root of, `C:\` when
    /// it is missing or not a drive.
    #[test]
    fn the_system_drive_is_its_root() {
        assert_eq!(system_drive(Some("C:")), r"C:\");
        assert_eq!(system_drive(Some("D:")), r"D:\");
        assert_eq!(system_drive(None), r"C:\");
        assert_eq!(system_drive(Some("")), r"C:\");
        assert_eq!(system_drive(Some(r"\\server")), r"C:\");
    }

    /// `g /` opens the Places card, the drives on it standing for the root
    /// a Windows machine does not have.
    #[test]
    fn g_slash_is_the_places_card() {
        let km = Registry::defaults();
        let seq = parse_sequence("g /").expect("chord");
        assert_eq!(
            km.holder(Context::Files, &seq).map(|b| b.command),
            Some(Command::MountManager)
        );
    }

    /// A row takes its first program that is there, else its last; a
    /// builtin is always there.
    #[test]
    fn a_row_takes_the_first_program_that_is_there() {
        let edit: &[&'static str] = &[r#"code --wait "$@""#, r#"notepad "$@""#];
        assert_eq!(pick(edit, &|p| p == "code"), r#"code --wait "$@""#);
        assert_eq!(pick(edit, &|p| p == "notepad"), r#"notepad "$@""#);
        assert_eq!(pick(edit, &|_| false), r#"notepad "$@""#, "else the last");
        assert_eq!(
            pick(&["builtin:shell-open"], &|_| false),
            "builtin:shell-open"
        );
    }

    /// The table the app holds: one command per row, the terminal and the
    /// editor whichever this machine has — the runner has `cmd` and Notepad
    /// whatever else it has — and every name a rule gives is an opener.
    #[test]
    fn the_shipped_openers_are_what_this_machine_has() {
        let c = Config::default();
        let command = |id: &str| c.opener(id).map(|o| o.command.clone()).unwrap_or_default();
        assert!(
            ["wt -d \"$1\"", "cmd /K cd /d \"$1\""].contains(&command("terminal-here").as_str()),
            "{}",
            command("terminal-here")
        );
        assert!(
            ["code --wait \"$@\"", "notepad \"$@\""].contains(&command("edit").as_str()),
            "{}",
            command("edit")
        );
        assert_eq!(command("open"), "builtin:shell-open");
        assert!(c.opener("bulk-rename").is_some_and(|o| o.block));
        assert!(on_path(
            &std::env::var_os("PATH").unwrap_or_default(),
            "cmd"
        ));
        for rule in &c.rules {
            for name in &rule.openers {
                assert!(c.opener(name).is_some(), "{rule:?} names a missing {name}");
            }
        }
        let names =
            |v: Vec<&Opener>| -> Vec<String> { v.into_iter().map(|o| o.name.clone()).collect() };
        assert_eq!(
            names(c.openers_for("cat.png", "image/png", false))[0],
            "open"
        );
        assert_eq!(
            names(c.openers_for("notes.md", "text/markdown", false)),
            ["zed", "edit", "open", "terminal-at"]
        );
    }
}
