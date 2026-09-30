//! What a fresh install ships on macOS
//! (`plans/other-platforms/05-defaults-and-config.md` §2.1, §2.3, §6).
//!
//! Linux's tables are Brian's machine: `setsid uwsm-app` to hand a program
//! to the compositor, `xdg-open`, delightviewer, pinta, his wallpaper and
//! AVIF helpers, his server's mounts. None of that is on a Mac. These are
//! what a fresh Mac has, in the same shell-string form (`$SHELL -c`, `$1` and
//! `$@`), plus Zed and mpv where they are installed; a missing one is the
//! ordinary "opener not found". Detaching needs no prefix here: the shared
//! Unix spawn puts every launch in a process group of its own (M2.16).
//!
//! The rules are Linux's row for row, with the system's default app (`open`)
//! where Linux has delightviewer: a picture, a video, a song, a PDF, a 3D
//! model, a font and a toolpath each open in whatever the Mac opens them
//! with, and `O` still offers Preview for a picture and mpv for a video. The
//! bookmarks are the four the plan gives keys to; Brian's server mounts and
//! hosts stay Linux's.

/// Openers a rule can name: id, command, blocking, description.
///
/// `edit` wants a terminal to run `$EDITOR` in: `$TERMINAL`, else Ghostty,
/// when either is on `PATH`, and Terminal.app when neither is.
/// `TERMINAL_APP` is delightfile's own variable, since `$TERMINAL` names a
/// program and `open -a` wants an application's name.
pub const OPENERS: &[(&str, &str, bool, &str)] = &[
    (
        "edit",
        r#"command -v "${TERMINAL:-ghostty}" >/dev/null 2>&1 && exec "${TERMINAL:-ghostty}" -e "${EDITOR:-vi}" "$@"; exec open -a Terminal "$@""#,
        false,
        "Edit in $EDITOR",
    ),
    (
        "zed",
        r#"zed "$@" 2>/dev/null || open -a Zed "$@""#,
        false,
        "Open in Zed",
    ),
    (
        "zed-workspace",
        r#"zed "$1" 2>/dev/null || open -a Zed "$1""#,
        false,
        "Open folder in Zed",
    ),
    (
        "terminal-here",
        r#"open -a "${TERMINAL_APP:-Terminal}" "$1""#,
        false,
        "Terminal here",
    ),
    (
        "terminal-at",
        r#"open -a "${TERMINAL_APP:-Terminal}" "$(dirname "$1")""#,
        false,
        "Terminal at file",
    ),
    (
        "open-in-chrome",
        r#"open -a "Google Chrome" "$@""#,
        false,
        "Open in Chrome",
    ),
    ("edit-image", r#"open -a Preview "$@""#, false, "Edit image"),
    (
        "bulk-rename",
        r#"zed --new --wait "$@""#,
        true,
        "Bulk rename in Zed",
    ),
    ("open", r#"open "$1""#, false, "Open"),
    (
        "play",
        r#"command -v mpv >/dev/null 2>&1 && exec mpv --force-window "$@"; exec open "$@""#,
        false,
        "Play",
    ),
    ("extract", "builtin:extract", false, "Extract to folder"),
    (
        "extract-here",
        "builtin:extract-here",
        false,
        "Extract here",
    ),
    (
        "extract-merged",
        "builtin:extract-merged",
        false,
        "Extract all into one folder",
    ),
];

/// Opener rules, matched top-down: Linux's
/// ([`crate::config::DEFAULT_RULES`]) row for row, with `open` — the
/// system's default app — where Linux has delightviewer, and the openers a
/// Mac does not ship (delightviewer's editor, the wallpaper and AVIF
/// helpers) left out. An `open` a row would then name twice is named once.
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
    ("mime", "image/*", &["open", "edit-image", "terminal-at"]),
    ("mime", "video/*", &["open", "play", "terminal-at"]),
    ("mime", "audio/*", &["open", "terminal-at"]),
    ("mime", "application/pdf", &["open", "terminal-at"]),
    ("glob", "*/", &["open", "zed-workspace", "terminal-here"]),
    ("glob", "*", &["open", "terminal-at"]),
];

/// The `g` chord's bookmarks: key, path, description, in which-key order.
pub const BOOKMARKS: &[(&str, &str, &str)] = &[
    ("h", "~", "Go home"),
    ("c", "~/.config", "Go to ~/.config"),
    ("d", "~/Downloads", "Go to ~/Downloads"),
    ("w", "~/Work", "Go to ~/Work"),
];

/// Rows laid over the shipped keymap, before the user's `keymap.toml`:
/// context, keys, command id, description (05-defaults-and-config.md §3).
///
/// Command reads as `ctrl` here (df-app's `platform::keys::mods`), so these
/// are the Mac's Cmd chords: Cmd+C copies the selection's paths (`Y`) where
/// Linux's Ctrl+c closes a tab, Cmd+W closes the tab, Cmd+V pastes (`p`, the
/// yank or else the system clipboard), Cmd+Q quits, and in a field Cmd+C
/// copies from it where Linux's cancels it. Every other overlay keeps
/// Ctrl+c as its way out.
pub const KEYMAP_OVERRIDES: &[(&str, &str, &str, &str)] = &[
    (
        "files",
        "ctrl+c",
        "copy-to-clipboard",
        "Copy to the system clipboard",
    ),
    (
        "files",
        "ctrl+w",
        "close-tab",
        "Close tab, or quit if it is the last",
    ),
    ("files", "ctrl+v", "paste", "Paste"),
    ("files", "ctrl+q", "quit", "Quit"),
    (
        "input",
        "ctrl+c",
        "input-copy",
        "Copy the selection, or the whole line",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Opener};
    use crate::keymap::{parse_sequence, Command, Context, Registry};

    /// The Cmd chords a Mac expects, over the shipped keymap: Cmd+C in the
    /// file list copies to the system clipboard and in a field copies from
    /// it, Cmd+W closes a tab, Cmd+V pastes, Cmd+Q quits, and the overlays
    /// still close on Cmd+C.
    #[test]
    fn the_cmd_chords_mean_what_a_mac_means() {
        let km = Registry::defaults();
        let at = |context: Context, keys: &str| {
            let seq = parse_sequence(keys).expect("chord");
            km.lookup(context, seq[0])
        };
        assert_eq!(at(Context::Files, "ctrl+c"), Some(Command::CopyToClipboard));
        assert_eq!(at(Context::Files, "ctrl+w"), Some(Command::CloseTab));
        assert_eq!(at(Context::Files, "ctrl+v"), Some(Command::Paste));
        assert_eq!(at(Context::Files, "ctrl+q"), Some(Command::Quit));
        assert_eq!(at(Context::Input, "ctrl+c"), Some(Command::InputCopy));
        assert_eq!(at(Context::Confirm, "ctrl+c"), Some(Command::OverlayClose));
        assert_eq!(at(Context::Help, "ctrl+c"), Some(Command::OverlayClose));
        assert_eq!(at(Context::Files, "Y"), Some(Command::CopyToClipboard));
    }

    fn names(openers: Vec<&Opener>) -> Vec<&str> {
        openers.into_iter().map(|o| o.name.as_str()).collect()
    }

    /// A fresh install on a Mac reads these tables: the `g` bookmarks are
    /// the four with keys, `~/Work` at `g w` as on Linux.
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
                ("c", "~/.config"),
                ("d", "~/Downloads"),
                ("w", "~/Work"),
            ]
        );
    }

    /// The rules as a Mac gets them: text in Zed first, then the terminal
    /// editor, the default app and a shell in the folder, as on Linux; what
    /// Linux sends to delightviewer — pictures, video, sound, PDFs — opens
    /// in the default app through `open`; archives extract; a folder opens,
    /// and nothing is ever an empty picker.
    #[test]
    fn opener_rules_match_by_mime_and_by_glob() {
        let c = Config::default();
        assert_eq!(names(c.openers_for("x.txt", "text/plain", false))[0], "zed");
        assert_eq!(
            names(c.openers_for("notes.md", "text/markdown", false)),
            vec!["zed", "edit", "open", "terminal-at"]
        );
        assert_eq!(
            names(c.openers_for("index.html", "text/html", false)),
            vec!["zed", "open-in-chrome", "edit", "open", "terminal-at"]
        );
        assert_eq!(
            names(c.openers_for("package.json", "application/json", false)),
            vec!["zed", "edit", "open", "terminal-at"]
        );
        // Where Linux has delightviewer, the default app: a 3D model and a
        // toolpath by name, ahead of the text rule their `text/plain` would
        // reach, and a font; then pictures, video, sound and PDFs by type.
        for (name, mime, openers) in [
            ("teapot.obj", "text/plain", vec!["open", "terminal-at"]),
            (
                "benchy.gcode",
                "text/plain",
                vec!["open", "edit", "terminal-at"],
            ),
            ("Inter.ttf", "font/ttf", vec!["open", "terminal-at"]),
            (
                "cat.png",
                "image/png",
                vec!["open", "edit-image", "terminal-at"],
            ),
            ("clip.mp4", "video/mp4", vec!["open", "play", "terminal-at"]),
            ("song.mp3", "audio/mpeg", vec!["open", "terminal-at"]),
            ("paper.pdf", "application/pdf", vec!["open", "terminal-at"]),
            (
                "mystery",
                "application/octet-stream",
                vec!["open", "terminal-at"],
            ),
        ] {
            assert_eq!(names(c.openers_for(name, mime, false)), openers, "{name}");
        }
        assert_eq!(
            names(c.openers_for("backup.tar.gz", "application/gzip", false)),
            vec!["extract", "extract-here", "extract-merged", "open"]
        );
        assert_eq!(
            names(c.openers_for("Work", "inode/directory", true)),
            vec!["open", "zed-workspace", "terminal-here"]
        );
        let open = c.opener("open").expect("open");
        assert_eq!(open.command, r#"open "$1""#);
    }

    /// `edit` is a terminal, never a wait, and the terminals are
    /// Terminal.app unless `TERMINAL_APP` names another.
    #[test]
    fn edit_opens_a_terminal_and_terminals_are_apps() {
        let c = Config::default();
        let edit = c.opener("edit").expect("edit");
        assert!(!edit.block);
        assert!(edit.command.contains(r#""${EDITOR:-vi}""#));
        assert!(edit.command.ends_with(r#"exec open -a Terminal "$@""#));
        let at = c.opener("terminal-at").expect("terminal-at");
        assert_eq!(
            at.command,
            r#"open -a "${TERMINAL_APP:-Terminal}" "$(dirname "$1")""#
        );
        assert!(c.opener("bulk-rename").is_some_and(|o| o.block));
    }

    /// Every opener a rule names is one this table has, so no rule offers a
    /// row `O` would drop.
    #[test]
    fn every_rule_names_an_opener_macos_ships() {
        for (_, pattern, openers) in RULES {
            for name in *openers {
                assert!(
                    OPENERS.iter().any(|(id, ..)| id == name),
                    "{pattern} names {name}"
                );
            }
        }
    }

    /// Nothing of Linux's desktop is left in a command: no `setsid`, no
    /// `uwsm-app`, no `xdg-open`.
    #[test]
    fn no_command_names_linux_tools() {
        for (id, command, ..) in OPENERS {
            for linux in ["setsid", "uwsm-app", "xdg-open", "zeditor"] {
                assert!(!command.contains(linux), "{id}: {command}");
            }
        }
    }
}
