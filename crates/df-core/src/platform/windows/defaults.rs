//! What a fresh install ships on Windows
//! (`plans/other-platforms/05-defaults-and-config.md` §2.2, §2.3, §6;
//! `04-windows.md` W4.40, W4.41, W4.43).
//!
//! **Enter does what a double-click in Explorer does.** Every rule's first
//! opener is the system's own "open" (`builtin:shell-open`), the program the
//! file is associated with, so a photo opens in Photos, a page in the
//! browser, a song in Media Player, and a file with no association in
//! Windows' "How do you want to open this?". `O` is where the alternatives
//! are, and only the ones this machine has: VS Code, Notepad++ and Notepad
//! for text, Paint for a picture, mpv and VLC for video and sound, 7-Zip's
//! window for an archive (after the extract built-ins), Explorer, VS Code and
//! a terminal for a folder. Every file's list ends in "Open with…", Windows'
//! own chooser, the escape hatch for anything the table did not think of. A
//! font is the one kind whose Enter is not "open": it is the shell's
//! `install` verb (`builtin:font-install`, W4.43), the Explorer menu's
//! "Install", with the Font Viewer on `O`.
//!
//! Openers are argv lines here, not shell strings: df-app's Windows opener
//! splits a command on whitespace, groups double quotes, and puts the paths
//! in for `$1`, `$@` and `$dir`, nothing else (W4.3).
//!
//! **A row is as many programs as can answer it, the best first** — `code`,
//! `notepad++`, then `notepad` to edit; `wt` then `cmd` for a terminal.
//! [`openers`] keeps, per row, the first whose program this machine has when
//! it is asked (when [`crate::config::Config::default`] builds the table,
//! once a start), and a row none of whose programs is here is not shipped at
//! all, so no rule offers it and `O` never shows a choice that would only
//! say "not found" (D5.4). A program is had when it is on `PATH` — under
//! the names Windows runs it by, `.exe` and VS Code's `.cmd` — or registered
//! in `App Paths`, where Notepad++, VLC and 7-Zip put themselves instead,
//! and then the row runs it by the full path registered there
//! ([`super::known::app_path`]). One installed while delightfile runs is
//! seen at the next start.
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
/// whose program this machine has), blocking, description.
pub const CANDIDATES: &[(&str, &[&str], bool, &str)] = &[
    ("open", &["builtin:shell-open"], false, "Open"),
    ("run", &["builtin:shell-open"], false, "Run"),
    (
        "open-with",
        &["builtin:shell-open-with"],
        false,
        "Open with…",
    ),
    ("vscode", &[r#"code "$@""#], false, "Open in VS Code"),
    (
        "notepad++",
        &[r#"notepad++ "$@""#],
        false,
        "Open in Notepad++",
    ),
    ("notepad", &[r#"notepad "$1""#], false, "Open in Notepad"),
    (
        "edit",
        &[r#"code "$@""#, r#"notepad++ "$@""#, r#"notepad "$1""#],
        false,
        "Edit",
    ),
    ("paint", &[r#"mspaint "$1""#], false, "Edit in Paint"),
    ("mpv", &[r#"mpv --force-window "$@""#], false, "Play in mpv"),
    ("vlc", &[r#"vlc "$@""#], false, "Play in VLC"),
    ("7-zip", &[r#"7zFM "$1""#], false, "Open in 7-Zip"),
    ("explorer", &[r#"explorer "$1""#], false, "Open in Explorer"),
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
    ("install-font", &["builtin:font-install"], false, "Install"),
    (
        "font-viewer",
        &["builtin:shell-open"],
        false,
        "Preview in Windows Font Viewer",
    ),
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
/// command whose program this machine has, and no row at all where it has
/// none of them.
pub fn openers() -> Vec<(&'static str, String, bool, &'static str)> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let locate = |program: &str| -> Option<Located> {
        if on_path(&path, program) {
            return Some(Located::OnPath);
        }
        known::app_path(program).map(|at| Located::At(at.to_string_lossy().into_owned()))
    };
    CANDIDATES
        .iter()
        .filter_map(|(id, commands, block, description)| {
            Some((*id, pick(commands, &locate)?, *block, *description))
        })
        .collect()
}

/// Where a program was found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Located {
    /// On `PATH`: the command runs it by name.
    OnPath,
    /// Registered in `App Paths` at this full path, which the command runs
    /// it by, since starting a process reads `PATH` and not the registry.
    At(String),
}

/// The first of `commands` whose program `locate` finds, as it should be
/// run; `None` when it finds none of them. A builtin needs no program.
fn pick(commands: &[&'static str], locate: &dyn Fn(&str) -> Option<Located>) -> Option<String> {
    commands.iter().find_map(|command| {
        if command.starts_with("builtin:") {
            return Some((*command).to_string());
        }
        let program = program(command)?;
        match locate(program)? {
            Located::OnPath => Some((*command).to_string()),
            Located::At(full) => Some(format!("\"{full}\"{}", &command[program.len()..])),
        }
    })
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

/// A program file — or an app execution alias, the empty stand-in under
/// `WindowsApps` a Store app (the new Paint, Windows Terminal) is run by,
/// which cannot be opened as a file and is not a link to one.
fn is_program(path: &Path) -> bool {
    crate::platform::process::is_executable(path)
        || (std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file())
            && path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe")))
}

/// Opener rules, matched top-down. Every file's first opener is the
/// system's own, then the alternatives this machine has, then "Open with…";
/// by name first, for the reason Linux's table gives (a `.obj` is
/// `text/plain`), then by type.
pub const RULES: &[(&str, &str, &[&str])] = &[
    ("glob", "bulk-rename.txt", &["bulk-rename"]),
    (
        "glob",
        "*.{ttf,otf,ttc}",
        &["install-font", "font-viewer", "open-with"],
    ),
    ("glob", "*.{exe,msi}", &["run", "open-with"]),
    ("glob", "*.{bat,cmd,ps1}", &["run", "edit", "open-with"]),
    ("glob", "*.{stl,obj,ply,3mf}", &["open", "open-with"]),
    (
        "glob",
        "*.{gcode,gco}",
        &["open", "vscode", "notepad++", "notepad", "open-with"],
    ),
    (
        "glob",
        "*.{zip,tar,tgz,gz,bz2,xz,zst,7z,rar,cbz,cbr}",
        &[
            "open",
            "extract",
            "extract-here",
            "extract-merged",
            "7-zip",
            "open-with",
        ],
    ),
    (
        "mime",
        "application/{zip,x-tar,gzip,x-bzip2,x-xz,zstd,x-7z-compressed,vnd.rar}",
        &[
            "open",
            "extract",
            "extract-here",
            "extract-merged",
            "7-zip",
            "open-with",
        ],
    ),
    // Paint cannot read a vector; VS Code can, as the text it is.
    ("mime", "image/svg+xml", &["open", "vscode", "open-with"]),
    ("mime", "image/*", &["open", "paint", "open-with"]),
    ("mime", "video/*", &["open", "mpv", "vlc", "open-with"]),
    ("mime", "audio/*", &["open", "mpv", "vlc", "open-with"]),
    ("mime", "application/pdf", &["open", "open-with"]),
    (
        "mime",
        "text/*",
        &["open", "vscode", "notepad++", "notepad", "open-with"],
    ),
    (
        "mime",
        "application/{json,ndjson,xml,javascript,x-shellscript,x-yaml,toml}",
        &["open", "vscode", "notepad++", "notepad", "open-with"],
    ),
    ("glob", "*/", &["explorer", "vscode", "terminal-here"]),
    // The fallback, so `O` is never empty.
    ("glob", "*", &["open", "open-with"]),
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
    rows.push(Bookmark::row("c", &drive, &format!("Drive {drive}")));
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

    /// A row takes its first program this machine has — by name when it is
    /// on `PATH`, by its full path when `App Paths` has it — and a row with
    /// none of its programs here is no row; a builtin is always here.
    #[test]
    fn a_row_takes_the_first_program_that_is_there() {
        let edit: &[&'static str] = &[r#"code "$@""#, r#"notepad++ "$@""#, r#"notepad "$1""#];
        let only = |name: &'static str| move |p: &str| (p == name).then_some(Located::OnPath);
        assert_eq!(pick(edit, &only("code")).as_deref(), Some(r#"code "$@""#));
        assert_eq!(
            pick(edit, &only("notepad")).as_deref(),
            Some(r#"notepad "$1""#)
        );
        assert_eq!(pick(edit, &|_| None), None, "nothing here, no row");
        let registered = |p: &str| {
            (p == "notepad++")
                .then(|| Located::At(r"C:\Program Files\Notepad++\notepad++.exe".into()))
        };
        assert_eq!(
            pick(edit, &registered).as_deref(),
            Some(r#""C:\Program Files\Notepad++\notepad++.exe" "$@""#)
        );
        assert_eq!(
            pick(&["builtin:shell-open"], &|_| None).as_deref(),
            Some("builtin:shell-open")
        );
    }

    fn names(openers: Vec<&Opener>) -> Vec<&str> {
        openers.into_iter().map(|o| o.name.as_str()).collect()
    }

    /// The table the app holds: Enter is the system's own open on every
    /// kind but a font, "Open with…" ends every file's list, a font
    /// installs, a folder offers Explorer, and every name a rule gives that
    /// the table has is an opener — the rest being programs this machine
    /// does not have, which the rule then skips.
    #[test]
    fn enter_is_what_a_double_click_does() {
        let c = Config::default();
        for (name, mime) in [
            ("cat.png", "image/png"),
            ("logo.svg", "image/svg+xml"),
            ("clip.mp4", "video/mp4"),
            ("song.mp3", "audio/mpeg"),
            ("paper.pdf", "application/pdf"),
            ("notes.txt", "text/plain"),
            ("index.html", "text/html"),
            ("data.json", "application/json"),
            ("backup.zip", "application/zip"),
            ("part.stl", "model/stl"),
            ("mystery", "application/octet-stream"),
        ] {
            let list = names(c.openers_for(name, mime, false));
            assert_eq!(list.first(), Some(&"open"), "{name}: {list:?}");
            assert_eq!(list.last(), Some(&"open-with"), "{name}: {list:?}");
        }
        for (name, mime) in [
            ("setup.exe", "application/x-msdownload"),
            ("go.ps1", "text/plain"),
        ] {
            let list = names(c.openers_for(name, mime, false));
            assert_eq!(list.first(), Some(&"run"), "{name}: {list:?}");
            assert_eq!(
                c.opener("run").and_then(Opener::builtin),
                Some("shell-open")
            );
        }
        let font = names(c.openers_for("face.ttf", "font/ttf", false));
        assert_eq!(font, ["install-font", "font-viewer", "open-with"]);
        assert_eq!(
            c.opener("install-font").and_then(Opener::builtin),
            Some("font-install")
        );
        let archive = names(c.openers_for("backup.zip", "application/zip", false));
        assert_eq!(
            &archive[1..4],
            ["extract", "extract-here", "extract-merged"]
        );
        let folder = names(c.openers_for("Work", "inode/directory", true));
        assert_eq!(folder.first(), Some(&"explorer"), "{folder:?}");
        assert_eq!(folder.last(), Some(&"terminal-here"), "{folder:?}");
        for gone in [
            "zed",
            "zed-workspace",
            "open-in-chrome",
            "edit-image",
            "play",
        ] {
            assert!(c.opener(gone).is_none(), "{gone} is not Windows'");
        }
        // The programs every Windows has are always rows.
        for there in [
            "open-with",
            "edit",
            "explorer",
            "terminal-here",
            "bulk-rename",
        ] {
            assert!(c.opener(there).is_some(), "{there}");
        }
        assert!(c.opener("bulk-rename").is_some_and(|o| o.block));
        assert!(!c.opener("edit").is_some_and(|o| o.block));
        for (_, _, openers) in RULES {
            for name in *openers {
                assert!(
                    CANDIDATES.iter().any(|(id, ..)| id == name),
                    "a rule names {name}, which no row is"
                );
            }
        }
    }

    /// The runner's machine: `cmd` is on `PATH`, so the terminal is there;
    /// `edit` is VS Code, Notepad++ or Notepad, whichever this machine has
    /// first; "Open with…" is the shell's own chooser.
    #[test]
    fn the_shipped_openers_are_what_this_machine_has() {
        let c = Config::default();
        let command = |id: &str| c.opener(id).map(|o| o.command.clone()).unwrap_or_default();
        let path = std::env::var_os("PATH").unwrap_or_default();
        assert!(on_path(&path, "cmd"));
        assert!(
            ["wt -d \"$1\"", "cmd /K cd /d \"$1\""].contains(&command("terminal-here").as_str()),
            "{}",
            command("terminal-here")
        );
        let edit = command("edit");
        assert!(
            edit.starts_with("code ") || edit.contains("notepad"),
            "{edit}"
        );
        assert_eq!(command("open"), "builtin:shell-open");
        assert_eq!(command("open-with"), "builtin:shell-open-with");
        eprintln!(
            "this machine's alternatives: {:?}",
            c.openers
                .iter()
                .map(|o| (&o.name, &o.command))
                .collect::<Vec<_>>()
        );
    }
}
