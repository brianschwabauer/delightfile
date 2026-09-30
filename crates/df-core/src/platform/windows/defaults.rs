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
//! and `edit-image`, which is `open` here. The bookmarks are §6's Windows
//! column: Brian's server mounts and hosts are no place on a Windows machine.

use std::path::Path;

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

/// The `g` chord's bookmarks: key, path, description, in which-key order.
///
/// `g c` is the folder `%APPDATA%` names by default, written from `~`
/// because a bookmark expands `~` and nothing else. Desktop and Documents
/// are on the letters a Mac gives them, so `g D` and `g o` mean one place on
/// both.
pub const BOOKMARKS: &[(&str, &str, &str)] = &[
    ("h", "~", "Go home"),
    ("c", "~/AppData/Roaming", "Go to %APPDATA%"),
    ("d", "~/Downloads", "Go to ~/Downloads"),
    ("w", "~/Work", "Go to ~/Work"),
    ("D", "~/Desktop", "Go to ~/Desktop"),
    ("o", "~/Documents", "Go to ~/Documents"),
];

/// Rows the platform lays over the shipped keymap: none, since Windows'
/// keymap is Linux's (05-defaults-and-config.md §3).
pub const KEYMAP_OVERRIDES: &[(&str, &str, &str, &str)] = &[];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Opener};

    /// A fresh install on Windows goes where §6's Windows column says, the
    /// server mounts Linux ships left out.
    #[test]
    fn default_bookmarks_are_the_goto_table() {
        let c = Config::default();
        let pairs: Vec<(&str, &str)> = c
            .goto
            .iter()
            .map(|b| (b.key.as_str(), b.path.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("h", "~"),
                ("c", "~/AppData/Roaming"),
                ("d", "~/Downloads"),
                ("w", "~/Work"),
                ("D", "~/Desktop"),
                ("o", "~/Documents"),
            ]
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
