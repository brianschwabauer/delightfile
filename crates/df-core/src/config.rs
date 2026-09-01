//! Configuration: `~/.config/delightfile/{delightfile,keymap,theme}.toml`.
//!
//! Three rules from PLAN §3, and everything here follows from them.
//!
//! **Defaults ARE Brian's current yazi config.** A fresh install is day-one
//! home: ratio `[1,4,3]`, dir-first case-insensitive alphabetical sort, the
//! size linemode, hidden files off, `scrolloff = 5`, 10/10 task workers, the
//! nine goto bookmarks, the opener rules from `yazi.toml`, and the nineteen
//! custom directory icons from `theme.toml` (PLAN §3 says twenty; the file has
//! nineteen, and the file wins). So the constants below are not
//! "sensible defaults" chosen in the abstract — they are a transcription, and
//! the tests assert they still match.
//!
//! **A missing file is silence.** Not a warning, not an error: the shipped
//! config *is* the config, and a user who has never written one has not done
//! anything wrong.
//!
//! **A bad line warns and the good ones still apply.** Handled by
//! [`crate::toml`], which reports [`ConfigWarning`]s with a file and a line
//! rather than failing the document. A typo in a colour must not cost you your
//! bookmarks.
//!
//! ## `delightfile.toml`
//!
//! ```toml
//! [mgr]
//! ratio = [1, 4, 3]
//! sort_by = "alphabetical"   # alphabetical natural extension size mtime btime random none
//! sort_dir_first = true
//! sort_sensitive = false
//! sort_reverse = false
//! linemode = "size"          # size permissions btime mtime owner none
//! show_hidden = false
//! show_symlink = true
//! scrolloff = 5
//! folder_sizes = true      # recursive directory sizes in the size column
//!
//! [tasks]
//! micro_workers = 10
//! macro_workers = 10
//! bizarre_retry = 3
//!
//! [preview]
//! tab_size = 2
//! max_text_bytes = 1048576   # 1 MiB
//! max_hex_bytes = 65536      # 64 KiB
//! image_quality = 80         # 0–100, for the shared thumbnail cache
//! wrap = false
//!
//! # Bookmarks for the `g` chord. Writing this table replaces the shipped one
//! # outright — otherwise a bookmark could never be removed.
//! [goto]
//! h = "~"
//! w = "~/Work"
//!
//! # A named opener. `block = true` means delightfile waits for it.
//! [opener.zed]
//! command = 'setsid uwsm-app -- zeditor "$@"'
//! desc = "Open in Zed"
//! block = false
//!
//! # Rules are matched top-down; the first match wins, and its `use` list is
//! # the order the `O` picker offers. Rules written here are *prepended* to the
//! # shipped ones, so the fallback stays reachable.
//! [[open.rules]]
//! mime = "image/*"
//! use = ["delightviewer", "reveal"]
//!
//! [[open.rules]]
//! glob = "*.{stl,obj}"
//! use = ["delightviewer"]
//! ```
//!
//! ## `theme.toml`
//!
//! ```toml
//! [flavor]
//! dark = "catppuccin-mocha"
//!
//! [palette]
//! accent = "#89b4fa"
//!
//! [[icon.dir]]
//! name = "Work"          # a glob; matched against the full path if it has a `/`
//! text = ""
//! fg = "#f7768e"
//!
//! # The same rule for files, matched against the file *name*. Written rules
//! # are checked before the built-in per-kind table, first match wins.
//! [[icon.file]]
//! name = "*.blend"
//! text = "󰂫"
//! fg = "#fab387"
//! ```

use std::path::{Path, PathBuf};

use crate::toml::{self, ConfigWarning, Table, Value};

// ── Constants, every one of them a transcription of the yazi config ─────────

/// Parent | list | preview, PLAN §2. The list is the pane you live in and the
/// preview is the one you glance at; 1:4:3 is what those two facts weigh out to
/// on a 16:9 screen, and it is what Brian's yazi has been set to for years.
pub const DEFAULT_RATIO: [u16; 3] = [1, 4, 3];

/// Rows kept between the cursor and the edge of the list before it scrolls.
/// Five is about a third of a short list — enough that the next few files are
/// always visible, few enough that the cursor still reaches the bottom row.
pub const DEFAULT_SCROLLOFF: usize = 5;

/// Workers for the small, latency-sensitive jobs (stat, mime, thumbnails) and
/// for the big ones (copies, moves, deletes). 10/10 as configured: enough
/// parallelism to saturate an NVMe queue, not so much that a directory of
/// 200k files spawns a thread storm.
pub const DEFAULT_MICRO_WORKERS: usize = 10;
pub const DEFAULT_MACRO_WORKERS: usize = 10;

/// How many times a task retries an operation that failed for a reason that
/// looks transient (a mount waking up, an NFS hiccup). Three is yazi's number
/// and the usual one: enough for a stall, short enough that a genuine failure
/// still surfaces while the user is still looking at the screen.
pub const DEFAULT_BIZARRE_RETRY: u32 = 3;

/// How wide a tab renders in the preview pane.
///
/// Two, not eight: the pane is a *glance*, sharing a third of the window with
/// the list, and eight-column indentation spends half of that on whitespace
/// before the first deeply-nested line has said anything. It is df-app's
/// `TAB_SIZE` and the number Brian's editors are set to.
pub const DEFAULT_TAB_SIZE: usize = 2;

/// JPEG quality, 0–100, for the thumbnail delightfile writes back to the shared
/// yazi-compatible cache (PLAN §6).
///
/// 80 is dv-media's own `ThumbnailOpts::default`, which is what delightviewer
/// writes with. One cache should not have two programs writing it at two
/// qualities, so the default is theirs and the key exists for the person who
/// wants smaller files.
pub const DEFAULT_IMAGE_QUALITY: u8 = 80;

/// The `g` chord's bookmarks, in which-key order: key, path, description.
/// `~` is expanded at use time so `$HOME` can move.
pub const DEFAULT_BOOKMARKS: &[(&str, &str, &str)] = &[
    ("h", "~", "Go home"),
    ("c", "~/.config", "Go to ~/.config"),
    ("d", "~/Downloads", "Go to ~/Downloads"),
    ("w", "~/Work", "Go to ~/Work"),
    ("s", "/mnt/schwabserverroot", "Go to the server"),
    ("p", "/mnt/schwabserverroot/plex", "Go to plex"),
    (
        "a",
        "/mnt/schwabserverroot/files/Projects",
        "Go to the archive",
    ),
    ("1", "sftp://showandtour1", "Go to showandtour1"),
    ("2", "sftp://showandtour2", "Go to showandtour2"),
];

/// Openers a rule can name: id, command, blocking, description.
///
/// The `setsid uwsm-app --` prefix is how a launched program is detached from
/// delightfile's own process group and handed to the compositor's scope — kill
/// the file manager and the editor you opened from it stays up. `block = true`
/// means delightfile waits, which only `$EDITOR` and bulk-rename want.
///
/// `builtin:` is not a shell command: it names a job delightfile does itself.
/// `builtin:extract` fills the gap PLAN §6 calls out — yazi shells out to a
/// plugin for this, and archives get no extract rule at all in the config being
/// ported, which is the one thing the rules were missing.
pub const DEFAULT_OPENERS: &[(&str, &str, bool, &str)] = &[
    ("edit", r#"${EDITOR:-vi} "$@""#, true, "$EDITOR"),
    (
        "zed",
        r#"setsid uwsm-app -- zeditor "$@" >/dev/null 2>&1"#,
        false,
        "Open in Zed",
    ),
    (
        "zed-workspace",
        r#"setsid uwsm-app -- zeditor "$1" >/dev/null 2>&1"#,
        false,
        "Open as a Zed workspace",
    ),
    (
        "terminal-here",
        r#"setsid uwsm-app -- "${TERMINAL:-ghostty}" --working-directory="$1" >/dev/null 2>&1"#,
        false,
        "Open a terminal here",
    ),
    (
        "open-in-chrome",
        r#"setsid uwsm-app -- google-chrome-stable "$@" >/dev/null 2>&1"#,
        false,
        "Open in Chrome",
    ),
    (
        "delightviewer",
        r#"setsid uwsm-app -- delightviewer --autoplay "$1" >/dev/null 2>&1"#,
        false,
        "Open",
    ),
    (
        "delightviewer-edit",
        r#"setsid uwsm-app -- delightviewer --edit "$1" >/dev/null 2>&1"#,
        false,
        "Edit",
    ),
    (
        "edit-image",
        r#"setsid uwsm-app -- pinta "$@" >/dev/null 2>&1"#,
        false,
        "Edit in Pinta",
    ),
    (
        "set-wallpaper",
        r#"system-cmd-wallpaper-set "$1""#,
        false,
        "Set as wallpaper",
    ),
    (
        "optimize-avif",
        r#"system-cmd-image-optimize-yazi "$@""#,
        false,
        "Optimize to AVIF",
    ),
    (
        "bulk-rename",
        r#"zeditor --new --wait "$@""#,
        true,
        "Bulk rename in Zed",
    ),
    ("open", r#"xdg-open "$1""#, false, "Open"),
    ("reveal", r#"xdg-open "$(dirname "$1")""#, false, "Reveal"),
    ("play", r#"mpv --force-window "$@""#, false, "Play in mpv"),
    ("extract", "builtin:extract", false, "Extract here"),
];

/// Opener rules, matched top-down. Transcribed from `yazi.toml`'s
/// `[[open.prepend_rules]]`, plus the archive rules PLAN §6 asks for.
///
/// The by-name rules come first for the reason the yazi config gives: an `.obj`
/// and a `.ply` are `text/plain` and an `.stl` is `application/octet-stream`,
/// so a mime rule would send half the 3D formats to an editor and half to
/// nothing. A `.gcode` is `text/plain` to every detector on the machine, which
/// would open a 40 MB toolpath in a text editor.
const DEFAULT_RULES: &[(&str, &str, &[&str])] = &[
    ("glob", "bulk-rename.txt", &["bulk-rename"]),
    ("glob", "*.{stl,obj,ply,3mf}", &["delightviewer", "reveal"]),
    (
        "glob",
        "*.{gcode,gco}",
        &["delightviewer", "edit", "open", "reveal"],
    ),
    (
        "glob",
        "*.{ttf,otf,ttc}",
        &["delightviewer", "open", "reveal"],
    ),
    (
        "glob",
        "*.{zip,tar,tgz,gz,bz2,xz,zst,7z,rar,cbz,cbr}",
        &["extract", "reveal"],
    ),
    (
        "mime",
        "application/{zip,x-tar,gzip,x-bzip2,x-xz,zstd,x-7z-compressed,vnd.rar}",
        &["extract", "reveal"],
    ),
    (
        "mime",
        "text/html",
        &["edit", "zed", "open-in-chrome", "reveal"],
    ),
    ("mime", "text/*", &["edit", "zed", "reveal"]),
    (
        "mime",
        "application/{json,ndjson,xml,javascript,x-shellscript,x-yaml,toml}",
        &["edit", "zed", "reveal"],
    ),
    (
        "mime",
        "image/*",
        &[
            "delightviewer",
            "delightviewer-edit",
            "optimize-avif",
            "reveal",
            "set-wallpaper",
            "edit-image",
        ],
    ),
    (
        "mime",
        "video/*",
        &["delightviewer", "delightviewer-edit", "play", "reveal"],
    ),
    (
        "mime",
        "audio/*",
        &["delightviewer", "delightviewer-edit", "reveal"],
    ),
    (
        "mime",
        "application/pdf",
        &["delightviewer", "delightviewer-edit", "reveal"],
    ),
    ("glob", "*/", &["open", "zed-workspace", "terminal-here"]),
    // The fallback. yazi leaves this implicit; writing it down means the picker
    // is never empty, which is the one state `O` must not have.
    ("glob", "*", &["open", "reveal"]),
];

/// The twenty directory icons from `theme.toml`: glob, glyph, colour.
const DEFAULT_DIR_ICONS: &[(&str, char, Option<&str>)] = &[
    ("Audio Books", '\u{f0067}', Some("#68e0cc")),
    ("Archives", '\u{f174f}', None),
    ("Backup", '\u{f174f}', None),
    ("brian", '\u{f015}', None),
    ("Code", '\u{e70c}', Some("#f7768e")),
    ("Documents", '\u{f0219}', None),
    ("Final Media", '\u{f00f}', Some("#d16d9e")),
    ("Films", '\u{f0fce}', Some("#68e0cc")),
    ("Games", '\u{f1393}', Some("#68e0cc")),
    ("Media", '\u{f00f}', Some("#e0af68")),
    ("Music", '\u{f025}', Some("#e0af68")),
    ("Pictures", '\u{f03e}', Some("#e0af68")),
    ("plex", '\u{f06ba}', Some("#68e0cc")),
    ("Projects", '\u{f0a98}', Some("#acf776")),
    ("Templates", '\u{f4d0}', None),
    ("TV Shows", '\u{f0448}', Some("#68e0cc")),
    ("Videos", '\u{f03d}', Some("#e0af68")),
    ("Windows", '\u{e70f}', None),
    ("Work", '\u{f0b1}', Some("#f7768e")),
];

/// catppuccin-mocha, which PLAN §3 pins for both the dark and the light flavor
/// (§1: no light theme initially). Named so `theme.toml` can override a single
/// colour without restating the palette.
const DEFAULT_PALETTE: &[(&str, &str)] = &[
    ("rosewater", "#f5e0dc"),
    ("flamingo", "#f2cdcd"),
    ("pink", "#f5c2e7"),
    ("mauve", "#cba6f7"),
    ("red", "#f38ba8"),
    ("maroon", "#eba0ac"),
    ("peach", "#fab387"),
    ("yellow", "#f9e2af"),
    ("green", "#a6e3a1"),
    ("teal", "#94e2d5"),
    ("sky", "#89dceb"),
    ("sapphire", "#74c7ec"),
    ("blue", "#89b4fa"),
    ("lavender", "#b4befe"),
    ("text", "#cdd6f4"),
    ("subtext1", "#bac2de"),
    ("subtext0", "#a6adc8"),
    ("overlay2", "#9399b2"),
    ("overlay1", "#7f849c"),
    ("overlay0", "#6c7086"),
    ("surface2", "#585b70"),
    ("surface1", "#45475a"),
    ("surface0", "#313244"),
    ("base", "#1e1e2e"),
    ("mantle", "#181825"),
    ("crust", "#11111b"),
];

// ── The model ───────────────────────────────────────────────────────────────

/// What the list is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    #[default]
    Alphabetical,
    Natural,
    Extension,
    Size,
    Mtime,
    Btime,
    Random,
    None,
}

impl SortBy {
    pub fn from_name(name: &str) -> Option<SortBy> {
        Some(match name {
            "alphabetical" => SortBy::Alphabetical,
            "natural" => SortBy::Natural,
            "extension" => SortBy::Extension,
            "size" => SortBy::Size,
            "mtime" | "modified" => SortBy::Mtime,
            "btime" | "created" => SortBy::Btime,
            "random" => SortBy::Random,
            "none" => SortBy::None,
            _ => return None,
        })
    }
}

/// The second column of a row: what a file's one number is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineMode {
    #[default]
    Size,
    Permissions,
    Btime,
    Mtime,
    Owner,
    None,
}

impl LineMode {
    pub fn from_name(name: &str) -> Option<LineMode> {
        Some(match name {
            "size" => LineMode::Size,
            "permissions" | "perms" => LineMode::Permissions,
            "btime" | "created" => LineMode::Btime,
            "mtime" | "modified" => LineMode::Mtime,
            "owner" => LineMode::Owner,
            "none" => LineMode::None,
            _ => return None,
        })
    }
}

/// PLAN §2's layout and sorting.
#[derive(Debug, Clone, PartialEq)]
pub struct MgrConfig {
    pub ratio: [u16; 3],
    pub sort_by: SortBy,
    pub sort_dir_first: bool,
    pub sort_sensitive: bool,
    pub sort_reverse: bool,
    pub linemode: LineMode,
    pub show_hidden: bool,
    pub show_symlink: bool,
    pub scrolloff: usize,
    /// Whether directories get a recursive size in the size column (PLAN §7.3).
    ///
    /// On, because a size column that says nothing for half its rows is a
    /// column you stop reading. Off is for the machine where `~` is an NFS
    /// mount and a background walk is a bill somebody pays — the flag exists so
    /// that person does not have to choose between the size column and their
    /// network.
    pub folder_sizes: bool,
}

impl Default for MgrConfig {
    fn default() -> MgrConfig {
        MgrConfig {
            ratio: DEFAULT_RATIO,
            sort_by: SortBy::Alphabetical,
            sort_dir_first: true,
            sort_sensitive: false,
            sort_reverse: false,
            linemode: LineMode::Size,
            show_hidden: false,
            show_symlink: true,
            scrolloff: DEFAULT_SCROLLOFF,
            folder_sizes: true,
        }
    }
}

/// PLAN §5's worker pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasksConfig {
    pub micro_workers: usize,
    pub macro_workers: usize,
    pub bizarre_retry: u32,
}

impl Default for TasksConfig {
    fn default() -> TasksConfig {
        TasksConfig {
            micro_workers: DEFAULT_MICRO_WORKERS,
            macro_workers: DEFAULT_MACRO_WORKERS,
            bizarre_retry: DEFAULT_BIZARRE_RETRY,
        }
    }
}

/// PLAN §6's preview pane, as far as it is a matter of taste or of a cap.
///
/// The caps were named constants in [`crate::preview::job`] and the tab width a
/// constant in df-app; they are here as well because they are the four numbers
/// a person actually wants to move. Somebody reading minified JSON wants a
/// bigger `max_text_bytes`; somebody on a slow sshfs wants a smaller one. The
/// defaults are the constants, so a config that says nothing changes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewConfig {
    /// Spaces a `\t` renders as in the text previewer.
    pub tab_size: usize,
    /// The most of a text file that is read and highlighted.
    pub max_text_bytes: usize,
    /// The most of a binary file that is turned into a hexdump. Smaller than
    /// the text cap because a hexdump is four screen columns per byte.
    pub max_hex_bytes: usize,
    /// JPEG quality, 0–100, for the thumbnail written back to the shared cache.
    pub image_quality: u8,
    /// Soft-wrap long lines in the text previewer instead of letting them run
    /// off the edge. Off, because a wrapped line of minified JS is a wall and
    /// the pane is a glance: the horizontal cut tells you the line is long,
    /// which is information the wrap destroys.
    pub wrap: bool,
}

impl Default for PreviewConfig {
    fn default() -> PreviewConfig {
        PreviewConfig {
            tab_size: DEFAULT_TAB_SIZE,
            max_text_bytes: crate::preview::TEXT_BYTES,
            max_hex_bytes: crate::preview::HEX_BYTES,
            image_quality: DEFAULT_IMAGE_QUALITY,
            wrap: false,
        }
    }
}

/// One `g <key>` destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    /// The chord after `g` — a single key, written the way `keymap.toml` writes
    /// keys (`h`, `1`, `space`).
    pub key: String,
    /// Written as configured, `~` and all, so `$HOME` can move under it.
    pub path: String,
    pub description: String,
}

impl Bookmark {
    /// The path with a leading `~` replaced by `$HOME`. Left alone when there
    /// is no `$HOME` or the path is a URL (`sftp://…`), which the vfs resolves.
    pub fn expanded_path(&self) -> String {
        let Some(rest) = self.path.strip_prefix('~') else {
            return self.path.clone();
        };
        match std::env::var_os("HOME") {
            Some(home) => format!("{}{}", home.to_string_lossy(), rest),
            None => self.path.clone(),
        }
    }
}

/// The shipped bookmark table, as [`Bookmark`]s.
pub fn default_bookmarks() -> Vec<Bookmark> {
    DEFAULT_BOOKMARKS
        .iter()
        .map(|(key, path, description)| Bookmark {
            key: (*key).to_string(),
            path: (*path).to_string(),
            description: (*description).to_string(),
        })
        .collect()
}

/// A named way to open a file (PLAN §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opener {
    pub name: String,
    /// A shell command with `$1`/`$@`, or `builtin:<job>` for something
    /// delightfile does itself.
    pub command: String,
    /// Wait for it to finish (and hold the UI) rather than detaching.
    pub block: bool,
    /// What the `O` picker calls it.
    pub description: String,
}

impl Opener {
    /// The job name when this is a built-in rather than a shell command.
    pub fn builtin(&self) -> Option<&str> {
        self.command.strip_prefix("builtin:")
    }
}

/// What a rule matches on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    /// Matched against the mime type, with `*` and `{a,b}` (`image/*`).
    Mime(String),
    /// Matched against the file name, or the full path when the pattern
    /// contains a `/`. A trailing `/` means "a directory" (`*/`).
    Glob(String),
}

/// One `[[open.rules]]` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRule {
    pub matcher: Matcher,
    /// Opener names, in the order the `O` picker offers them. The first is what
    /// plain `o`/`Enter` runs.
    pub openers: Vec<String>,
}

/// Everything `delightfile.toml` says.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub mgr: MgrConfig,
    pub tasks: TasksConfig,
    pub preview: PreviewConfig,
    pub goto: Vec<Bookmark>,
    pub openers: Vec<Opener>,
    pub rules: Vec<OpenRule>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            mgr: MgrConfig::default(),
            tasks: TasksConfig::default(),
            preview: PreviewConfig::default(),
            goto: default_bookmarks(),
            openers: DEFAULT_OPENERS
                .iter()
                .map(|(name, command, block, description)| Opener {
                    name: (*name).to_string(),
                    command: (*command).to_string(),
                    block: *block,
                    description: (*description).to_string(),
                })
                .collect(),
            rules: DEFAULT_RULES
                .iter()
                .map(|(kind, pattern, openers)| OpenRule {
                    matcher: if *kind == "mime" {
                        Matcher::Mime((*pattern).to_string())
                    } else {
                        Matcher::Glob((*pattern).to_string())
                    },
                    openers: openers.iter().map(|o| (*o).to_string()).collect(),
                })
                .collect(),
        }
    }
}

impl Config {
    pub fn opener(&self, name: &str) -> Option<&Opener> {
        self.openers.iter().find(|o| o.name == name)
    }

    /// The openers for a file, first rule wins, in picker order (PLAN §6).
    ///
    /// `name` is the file name (or the path, for the glob rules that contain a
    /// `/`), `mime` its detected type, `is_dir` whether it is a directory —
    /// which is what makes the `*/` rule work without a separate matcher kind.
    /// An opener a rule names but the config does not define is skipped rather
    /// than being an empty row in the picker.
    pub fn openers_for(&self, name: &str, mime: &str, is_dir: bool) -> Vec<&Opener> {
        let dir_name = if is_dir {
            format!("{name}/")
        } else {
            name.to_string()
        };
        for rule in &self.rules {
            let hit = match &rule.matcher {
                Matcher::Mime(pattern) => !is_dir && glob_match(pattern, mime),
                Matcher::Glob(pattern) => glob_match(pattern, &dir_name),
            };
            if hit {
                return rule.openers.iter().filter_map(|n| self.opener(n)).collect();
            }
        }
        Vec::new()
    }

    /// Read `delightfile.toml` over the defaults.
    pub fn parse(text: &str, file: &Path) -> (Config, Vec<ConfigWarning>) {
        let mut doc = toml::parse(text, file);
        let mut config = Config::default();
        let mut warnings = std::mem::take(&mut doc.warnings);

        if let Some(mgr) = doc.table("mgr") {
            for entry in &mgr.entries {
                let value = &entry.value;
                let ok = match entry.key.as_str() {
                    "ratio" => read_ratio(value, &mut config.mgr.ratio),
                    "sort_by" => read_enum(value, SortBy::from_name, &mut config.mgr.sort_by),
                    "sort_dir_first" => read_bool(value, &mut config.mgr.sort_dir_first),
                    "sort_sensitive" => read_bool(value, &mut config.mgr.sort_sensitive),
                    "sort_reverse" => read_bool(value, &mut config.mgr.sort_reverse),
                    "linemode" => read_enum(value, LineMode::from_name, &mut config.mgr.linemode),
                    "show_hidden" => read_bool(value, &mut config.mgr.show_hidden),
                    "show_symlink" => read_bool(value, &mut config.mgr.show_symlink),
                    "scrolloff" => read_usize(value, &mut config.mgr.scrolloff),
                    "folder_sizes" => read_bool(value, &mut config.mgr.folder_sizes),
                    _ => Err(format!("unknown key `{}` in [mgr]", entry.key)),
                };
                if let Err(message) = ok {
                    warnings.push(ConfigWarning::new(file, entry.line, message));
                }
            }
        }

        if let Some(tasks) = doc.table("tasks") {
            for entry in &tasks.entries {
                let value = &entry.value;
                let ok = match entry.key.as_str() {
                    "micro_workers" => read_usize(value, &mut config.tasks.micro_workers),
                    "macro_workers" => read_usize(value, &mut config.tasks.macro_workers),
                    "bizarre_retry" => {
                        let mut n = config.tasks.bizarre_retry as usize;
                        let r = read_usize(value, &mut n);
                        config.tasks.bizarre_retry = n as u32;
                        r
                    }
                    _ => Err(format!("unknown key `{}` in [tasks]", entry.key)),
                };
                if let Err(message) = ok {
                    warnings.push(ConfigWarning::new(file, entry.line, message));
                }
            }
        }

        if let Some(preview) = doc.table("preview") {
            for entry in &preview.entries {
                let value = &entry.value;
                let ok = match entry.key.as_str() {
                    "tab_size" => read_usize(value, &mut config.preview.tab_size),
                    "max_text_bytes" => read_usize(value, &mut config.preview.max_text_bytes),
                    "max_hex_bytes" => read_usize(value, &mut config.preview.max_hex_bytes),
                    "image_quality" => read_quality(value, &mut config.preview.image_quality),
                    "wrap" => read_bool(value, &mut config.preview.wrap),
                    _ => Err(format!("unknown key `{}` in [preview]", entry.key)),
                };
                if let Err(message) = ok {
                    warnings.push(ConfigWarning::new(file, entry.line, message));
                }
            }
        }

        // Writing [goto] replaces the shipped table rather than adding to it:
        // there is no other way to *remove* a default bookmark, and a config
        // whose entries can only accumulate is one you cannot correct.
        if let Some(goto) = doc.table("goto") {
            let mut bookmarks = Vec::new();
            for entry in &goto.entries {
                match entry.value.as_str() {
                    Some(path) => bookmarks.push(Bookmark {
                        key: entry.key.clone(),
                        path: path.to_string(),
                        description: format!("Go to {path}"),
                    }),
                    None => warnings.push(ConfigWarning::new(
                        file,
                        entry.line,
                        format!(
                            "goto `{}`: expected a path (a string), found {}",
                            entry.key,
                            entry.value.type_name()
                        ),
                    )),
                }
            }
            config.goto = bookmarks;
        }

        for (name, table) in doc.tables_under("opener") {
            let Some(command) = table.get("command").and_then(Value::as_str) else {
                warnings.push(ConfigWarning::new(
                    file,
                    table.line,
                    format!("[opener.{name}] has no `command`"),
                ));
                continue;
            };
            let opener = Opener {
                name: name.to_string(),
                command: command.to_string(),
                block: table.get("block").and_then(Value::as_bool).unwrap_or(false),
                description: table
                    .get("desc")
                    .and_then(Value::as_str)
                    .unwrap_or(name)
                    .to_string(),
            };
            match config.openers.iter().position(|o| o.name == name) {
                Some(i) => config.openers[i] = opener,
                None => config.openers.push(opener),
            }
        }

        // Prepended, like yazi's `prepend_rules`, so the shipped fallback is
        // still there under whatever the user added.
        let mut user_rules = Vec::new();
        for table in doc.tables_named("open.rules") {
            match parse_rule(table) {
                Ok(rule) => user_rules.push(rule),
                Err(message) => {
                    warnings.push(ConfigWarning::new(file, table.line, message));
                }
            }
        }
        if !user_rules.is_empty() {
            user_rules.append(&mut config.rules);
            config.rules = user_rules;
        }

        (config, warnings)
    }
}

fn parse_rule(table: &Table) -> Result<OpenRule, String> {
    let matcher = match (
        table.get("mime").and_then(Value::as_str),
        table.get("glob").and_then(Value::as_str),
    ) {
        (Some(mime), None) => Matcher::Mime(mime.to_string()),
        (None, Some(glob)) => Matcher::Glob(glob.to_string()),
        (Some(_), Some(_)) => {
            return Err("[[open.rules]] has both `mime` and `glob` — pick one".to_string())
        }
        (None, None) => return Err("[[open.rules]] has neither `mime` nor `glob`".to_string()),
    };
    let openers = table
        .get("use")
        .and_then(Value::as_str_array)
        .ok_or("[[open.rules]] needs `use = [\"opener\", …]`")?;
    if openers.is_empty() {
        return Err("[[open.rules]] `use` is empty".to_string());
    }
    Ok(OpenRule {
        matcher,
        openers: openers.into_iter().map(str::to_string).collect(),
    })
}

// ── Theme ───────────────────────────────────────────────────────────────────

/// An 8-bit-per-channel colour. Kept as sRGB bytes because that is what the
/// config file writes; df-app converts to whatever its painter wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    /// `#rgb` or `#rrggbb`.
    pub fn parse(text: &str) -> Option<Color> {
        let hex = text.strip_prefix('#')?;
        let byte = |s: &str| u8::from_str_radix(s, 16).ok();
        match hex.len() {
            3 => {
                let mut c = hex.chars();
                let mut nibble = || {
                    let d = c.next()?;
                    let v = d.to_digit(16)? as u8;
                    Some(v * 17) // `f` → `ff`, the usual CSS short-hex rule
                };
                Some(Color {
                    r: nibble()?,
                    g: nibble()?,
                    b: nibble()?,
                })
            }
            6 => Some(Color {
                r: byte(&hex[0..2])?,
                g: byte(&hex[2..4])?,
                b: byte(&hex[4..6])?,
            }),
            _ => None,
        }
    }
}

/// A directory that gets its own icon (PLAN §3, §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirIcon {
    /// A glob. Matched against the directory's name, or against the full path
    /// when the pattern contains a `/` — so both `Work` and `/mnt/*/plex` are
    /// sayable.
    pub pattern: String,
    pub text: char,
    pub fg: Option<Color>,
}

/// A file that gets its own icon, written as `[[icon.file]]` (PLAN §3, §8).
///
/// The same three fields [`DirIcon`] has and a separate type rather than a
/// shared one, because the two are matched against different things and grew
/// different defaults: a directory rule can address a *path* and ships with
/// nineteen entries transcribed from yazi, while a file rule is always about a
/// name and ships with none.
///
/// **Shipping none is the point.** The built-in file icons are per
/// [`crate::fs::FileKind`] and live in df-app, where the palette is — so they
/// re-tint when a user overrides `[palette]`, which a table of hex codes here
/// could never do. What `[[icon.file]]` adds is the escape hatch on top of
/// that: rules written here are checked first, so one line pins `*.blend` to
/// whatever glyph and colour you want without disturbing the other thousand
/// rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIcon {
    /// A glob, matched against the file's name — `*.rs`, `Cargo.toml`,
    /// `*.tar.*`.
    pub pattern: String,
    pub text: char,
    pub fg: Option<Color>,
}

/// Everything `theme.toml` says.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub flavor: String,
    /// Named colours, in declaration order so the theme browser can list them.
    pub palette: Vec<(String, Color)>,
    pub dir_icons: Vec<DirIcon>,
    /// User `[[icon.file]]` rules, in declaration order. Empty by default; see
    /// [`FileIcon`].
    pub file_icons: Vec<FileIcon>,
}

impl Default for Theme {
    fn default() -> Theme {
        Theme {
            flavor: "catppuccin-mocha".to_string(),
            palette: DEFAULT_PALETTE
                .iter()
                .filter_map(|(name, hex)| Color::parse(hex).map(|c| ((*name).to_string(), c)))
                .collect(),
            dir_icons: DEFAULT_DIR_ICONS
                .iter()
                .map(|(pattern, text, fg)| DirIcon {
                    pattern: (*pattern).to_string(),
                    text: *text,
                    fg: fg.and_then(Color::parse),
                })
                .collect(),
            file_icons: Vec::new(),
        }
    }
}

impl Theme {
    pub fn color(&self, name: &str) -> Option<Color> {
        self.palette
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| *c)
    }

    /// The icon for a directory, first rule wins. `path` is the full path and
    /// `name` its last component; a pattern with a `/` matches the path.
    pub fn dir_icon(&self, path: &str, name: &str) -> Option<&DirIcon> {
        self.dir_icons.iter().find(|i| {
            let subject = if i.pattern.contains('/') { path } else { name };
            glob_match(&i.pattern, subject)
        })
    }

    /// The user's icon rule for a file, first rule wins, or `None` to fall
    /// through to df-app's per-kind table.
    pub fn file_icon(&self, name: &str) -> Option<&FileIcon> {
        self.file_icons
            .iter()
            .find(|i| glob_match(&i.pattern, name))
    }

    pub fn parse(text: &str, file: &Path) -> (Theme, Vec<ConfigWarning>) {
        let mut doc = toml::parse(text, file);
        let mut theme = Theme::default();
        let mut warnings = std::mem::take(&mut doc.warnings);

        if let Some(flavor) = doc.table("flavor") {
            if let Some(entry) = flavor.entry("dark") {
                match entry.value.as_str() {
                    Some(name) => theme.flavor = name.to_string(),
                    None => warnings.push(ConfigWarning::new(
                        file,
                        entry.line,
                        "flavor.dark: expected a flavor name (a string)",
                    )),
                }
            }
        }

        if let Some(palette) = doc.table("palette") {
            for entry in &palette.entries {
                let parsed = entry.value.as_str().and_then(Color::parse);
                match parsed {
                    Some(color) => match theme.palette.iter().position(|(n, _)| *n == entry.key) {
                        Some(i) => theme.palette[i].1 = color,
                        None => theme.palette.push((entry.key.clone(), color)),
                    },
                    None => warnings.push(ConfigWarning::new(
                        file,
                        entry.line,
                        format!(
                            "palette `{}`: expected a colour like \"#89b4fa\"",
                            entry.key
                        ),
                    )),
                }
            }
        }

        // User icon rules go in front of the shipped ones: first match wins, so
        // prepending is the only way to override `Work` without deleting it.
        let mut icons = Vec::new();
        for table in doc.tables_named("icon.dir") {
            match parse_dir_icon(table) {
                Ok(icon) => icons.push(icon),
                Err(message) => {
                    warnings.push(ConfigWarning::new(file, table.line, message));
                }
            }
        }
        if !icons.is_empty() {
            icons.append(&mut theme.dir_icons);
            theme.dir_icons = icons;
        }

        // File rules ship empty, so there is nothing to prepend to: the list is
        // the user's, in the order they wrote it, and first match still wins.
        for table in doc.tables_named("icon.file") {
            match parse_file_icon(table) {
                Ok(icon) => theme.file_icons.push(icon),
                Err(message) => {
                    warnings.push(ConfigWarning::new(file, table.line, message));
                }
            }
        }

        (theme, warnings)
    }
}

fn parse_dir_icon(table: &Table) -> Result<DirIcon, String> {
    let (pattern, text, fg) = parse_icon_fields(table, "[[icon.dir]]")?;
    Ok(DirIcon { pattern, text, fg })
}

fn parse_file_icon(table: &Table) -> Result<FileIcon, String> {
    let (pattern, text, fg) = parse_icon_fields(table, "[[icon.file]]")?;
    Ok(FileIcon { pattern, text, fg })
}

/// The three fields both icon rules share, validated once. `what` is the table
/// name, so a warning names the table the user actually wrote.
fn parse_icon_fields(table: &Table, what: &str) -> Result<(String, char, Option<Color>), String> {
    let pattern = table
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{what} has no `name`"))?;
    let text = table
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{what} has no `text`"))?;
    let mut chars = text.chars();
    let (Some(glyph), None) = (chars.next(), chars.next()) else {
        return Err(format!("{what} `text = \"{text}\"` must be one character"));
    };
    let fg = match table.get("fg") {
        Some(value) => match value.as_str().and_then(Color::parse) {
            Some(color) => Some(color),
            None => return Err(format!("{what} `fg` must be a colour like \"#89b4fa\"")),
        },
        None => None,
    };
    Ok((pattern.to_string(), glyph, fg))
}

// ── Loading ─────────────────────────────────────────────────────────────────

/// Everything the config directory said, plus everything it got wrong.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub config: Config,
    pub theme: Theme,
    pub warnings: Vec<ConfigWarning>,
}

/// `$XDG_CONFIG_HOME/delightfile`, else `~/.config/delightfile` (PLAN §3).
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("delightfile"))
}

/// Load from the user's config directory, or the shipped defaults if there
/// isn't one.
pub fn load() -> Loaded {
    match config_dir() {
        Some(dir) => load_from_dir(&dir),
        None => Loaded::default(),
    }
}

/// Load from a specific directory. **A missing file is silence** (PLAN §3):
/// no warning, no error, just the defaults.
pub fn load_from_dir(dir: &Path) -> Loaded {
    let mut loaded = Loaded::default();
    let config_path = dir.join("delightfile.toml");
    if let Ok(text) = std::fs::read_to_string(&config_path) {
        let (config, mut warnings) = Config::parse(&text, &config_path);
        loaded.config = config;
        loaded.warnings.append(&mut warnings);
    }
    let theme_path = dir.join("theme.toml");
    if let Ok(text) = std::fs::read_to_string(&theme_path) {
        let (theme, mut warnings) = Theme::parse(&text, &theme_path);
        loaded.theme = theme;
        loaded.warnings.append(&mut warnings);
    }
    loaded
}

// ── Value readers ───────────────────────────────────────────────────────────

fn read_bool(value: &Value, dst: &mut bool) -> Result<(), String> {
    match value.as_bool() {
        Some(b) => {
            *dst = b;
            Ok(())
        }
        None => Err(format!(
            "expected true or false, found {}",
            value.type_name()
        )),
    }
}

fn read_usize(value: &Value, dst: &mut usize) -> Result<(), String> {
    match value.as_int() {
        Some(n) if n >= 0 => {
            *dst = n as usize;
            Ok(())
        }
        Some(n) => Err(format!("{n} must not be negative")),
        None => Err(format!(
            "expected a whole number, found {}",
            value.type_name()
        )),
    }
}

/// A 0–100 quality. Out of range is a warning and the default stands, rather
/// than a silent clamp: `image_quality = 300` is a typo, and a config that
/// quietly reinterprets a typo is one you cannot debug.
fn read_quality(value: &Value, dst: &mut u8) -> Result<(), String> {
    match value.as_int() {
        Some(n) if (0..=100).contains(&n) => {
            *dst = n as u8;
            Ok(())
        }
        Some(n) => Err(format!("{n} must be between 0 and 100")),
        None => Err(format!(
            "expected a whole number, found {}",
            value.type_name()
        )),
    }
}

fn read_enum<T: Copy>(
    value: &Value,
    from_name: fn(&str) -> Option<T>,
    dst: &mut T,
) -> Result<(), String> {
    let Some(name) = value.as_str() else {
        return Err(format!(
            "expected a name (a string), found {}",
            value.type_name()
        ));
    };
    match from_name(name) {
        Some(v) => {
            *dst = v;
            Ok(())
        }
        None => Err(format!("`{name}` is not one of the accepted values")),
    }
}

fn read_ratio(value: &Value, dst: &mut [u16; 3]) -> Result<(), String> {
    let Some(numbers) = value.as_int_array() else {
        return Err("ratio must be three whole numbers, e.g. [1, 4, 3]".to_string());
    };
    if numbers.len() != 3 {
        return Err(format!(
            "ratio must have exactly three entries, found {}",
            numbers.len()
        ));
    }
    if numbers.iter().any(|n| *n < 0 || *n > u16::MAX as i64) {
        return Err("ratio entries must be between 0 and 65535".to_string());
    }
    // All-zero would divide by zero when laying the panes out, and is the one
    // ratio a person could plausibly write by deleting the wrong thing.
    if numbers.iter().all(|n| *n == 0) {
        return Err("ratio cannot be all zeroes".to_string());
    }
    *dst = [numbers[0] as u16, numbers[1] as u16, numbers[2] as u16];
    Ok(())
}

// ── Glob matching ───────────────────────────────────────────────────────────

/// `*`, `?` and `{a,b,c}` alternatives, matched case-insensitively.
///
/// Case-insensitive on purpose: the yazi config this is ported from spells
/// every extension twice (`{stl,STL,obj,OBJ,…}`) because its matcher is not,
/// and a rule that opens `photo.JPG` in a text editor is not a rule anybody
/// wanted. Mime types are matched with the same function — `image/*` and
/// `application/{json,xml}` are the same shape of pattern.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let text = text.to_ascii_lowercase();
    for alternative in expand_braces(&pattern) {
        if wildcard_match(alternative.as_bytes(), text.as_bytes()) {
            return true;
        }
    }
    false
}

/// `a{b,c}d` → `["abd", "acd"]`. Only the first brace group is expanded per
/// pass, recursively, which handles nesting without a parser.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    let mut depth = 0i32;
    let mut close = None;
    for (i, c) in pattern[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        // An unbalanced brace is a literal brace, not an error: a file really
        // can be called `{draft}.txt`.
        return vec![pattern.to_string()];
    };
    let (head, tail) = (&pattern[..open], &pattern[close + 1..]);
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = open + 1;
    let body = &pattern[open + 1..close];
    for (i, c) in body.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                let piece = &pattern[start..open + 1 + i];
                out.extend(expand_braces(&format!("{head}{piece}{tail}")));
                start = open + 1 + i + 1;
            }
            _ => {}
        }
    }
    let piece = &pattern[start..close];
    out.extend(expand_braces(&format!("{head}{piece}{tail}")));
    out
}

/// `*` (any run) and `?` (one character), iteratively with backtracking — no
/// recursion, so a pathological pattern cannot blow the stack.
fn wildcard_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (Config, Vec<ConfigWarning>) {
        Config::parse(text, Path::new("delightfile.toml"))
    }

    /// PLAN §3: "Defaults ARE Brian's current yazi config." These are the
    /// numbers from `~/.config/yazi/yazi.toml`, and this test is the contract.
    #[test]
    fn defaults_are_the_yazi_config() {
        let c = Config::default();
        assert_eq!(c.mgr.ratio, [1, 4, 3]);
        assert_eq!(c.mgr.sort_by, SortBy::Alphabetical);
        assert!(c.mgr.sort_dir_first);
        assert!(!c.mgr.sort_sensitive);
        assert!(!c.mgr.sort_reverse);
        assert_eq!(c.mgr.linemode, LineMode::Size);
        assert!(!c.mgr.show_hidden);
        assert!(c.mgr.show_symlink);
        assert_eq!(c.mgr.scrolloff, 5);
        assert_eq!(c.tasks.micro_workers, 10);
        assert_eq!(c.tasks.macro_workers, 10);
        assert_eq!(c.tasks.bizarre_retry, 3);
    }

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
                ("s", "/mnt/schwabserverroot"),
                ("p", "/mnt/schwabserverroot/plex"),
                ("a", "/mnt/schwabserverroot/files/Projects"),
                ("1", "sftp://showandtour1"),
                ("2", "sftp://showandtour2"),
            ]
        );
    }

    #[test]
    fn theme_defaults_carry_the_custom_directory_icons() {
        let t = Theme::default();
        assert_eq!(t.flavor, "catppuccin-mocha");
        assert_eq!(t.dir_icons.len(), 19); // the yazi table, verbatim
        let work = t.dir_icon("/home/brian/Work", "Work").expect("Work");
        assert_eq!(work.fg, Color::parse("#f7768e"));
        assert_eq!(t.color("base"), Color::parse("#1e1e2e"));
        // Matching is case-insensitive, so `work` finds it too.
        assert!(t.dir_icon("/home/brian/work", "work").is_some());
        assert!(t.dir_icon("/home/brian/Nope", "Nope").is_none());
    }

    #[test]
    fn opener_rules_match_by_mime_and_by_glob() {
        let c = Config::default();
        let names =
            |v: Vec<&Opener>| -> Vec<String> { v.into_iter().map(|o| o.name.clone()).collect() };
        // Mime.
        assert_eq!(
            names(c.openers_for("cat.png", "image/png", false))
                .first()
                .map(String::as_str),
            Some("delightviewer")
        );
        assert_eq!(
            names(c.openers_for("notes.md", "text/markdown", false)),
            vec!["edit", "zed", "reveal"]
        );
        assert_eq!(
            names(c.openers_for("index.html", "text/html", false)),
            vec!["edit", "zed", "open-in-chrome", "reveal"]
        );
        // Glob, ahead of the mime rules on purpose: a .obj is text/plain.
        assert_eq!(
            names(c.openers_for("bracket.OBJ", "text/plain", false)),
            vec!["delightviewer", "reveal"]
        );
        // The archive rule PLAN §6 asks for, which yazi's config was missing.
        let extract = c.openers_for("backup.tar.gz", "application/gzip", false);
        assert_eq!(names(extract), vec!["extract", "reveal"]);
        assert_eq!(
            c.opener("extract").and_then(Opener::builtin),
            Some("extract")
        );
        // Directories match `*/`, whatever their mime.
        assert_eq!(
            names(c.openers_for("Work", "inode/directory", true)),
            vec!["open", "zed-workspace", "terminal-here"]
        );
        // …and nothing is ever an empty picker.
        assert_eq!(
            names(c.openers_for("mystery", "application/octet-stream", false)),
            vec!["open", "reveal"]
        );
    }

    #[test]
    fn glob_matching_handles_stars_braces_and_case() {
        assert!(glob_match("*.{png,jpg}", "cat.PNG"));
        assert!(glob_match("image/*", "image/avif"));
        assert!(glob_match("application/{json,xml}", "application/xml"));
        assert!(!glob_match("application/{json,xml}", "application/pdf"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("bulk-rename.txt", "bulk-rename.txt"));
        assert!(!glob_match("*.png", "png"));
        assert!(glob_match("?at", "cat"));
        assert!(!glob_match("?at", "goat"));
        // A brace group with one alternative is still a group…
        assert!(glob_match("{draft}.txt", "draft.txt"));
        // …but an unbalanced brace is a literal, because a file really can be
        // called `{draft.txt`.
        assert!(glob_match("{draft.txt", "{draft.txt"));
    }

    #[test]
    fn user_values_override_and_bad_ones_warn() {
        let (c, warnings) = parse(
            r#"
            [mgr]
            ratio = [2, 5, 4]
            show_hidden = true
            scrolloff = "lots"
            linemode = "owner"
            sort_by = "sideways"
            [tasks]
            macro_workers = 4
            "#,
        );
        assert_eq!(c.mgr.ratio, [2, 5, 4]);
        assert!(c.mgr.show_hidden);
        assert_eq!(c.mgr.linemode, LineMode::Owner);
        assert_eq!(c.tasks.macro_workers, 4);
        // The two bad lines warned; the good ones still applied, and the
        // values they failed to set kept their defaults.
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert_eq!(c.mgr.scrolloff, 5);
        assert_eq!(c.mgr.sort_by, SortBy::Alphabetical);
    }

    /// The `[preview]` defaults are the constants the code already used, so a
    /// config that says nothing about the pane changes nothing about it.
    #[test]
    fn preview_defaults_are_the_shipped_constants() {
        let p = Config::default().preview;
        assert_eq!(p.tab_size, 2);
        assert_eq!(p.max_text_bytes, 1024 * 1024);
        assert_eq!(p.max_hex_bytes, 64 * 1024);
        assert_eq!(p.image_quality, 80);
        assert!(!p.wrap);
        // …and they are literally those constants, not a second copy that can
        // drift from them.
        assert_eq!(p.max_text_bytes, crate::preview::TEXT_BYTES);
        assert_eq!(p.max_hex_bytes, crate::preview::HEX_BYTES);
    }

    #[test]
    fn a_preview_table_overrides_every_key() {
        let (c, warnings) = parse(
            r#"
            [preview]
            tab_size = 4
            max_text_bytes = 2048
            max_hex_bytes = 512
            image_quality = 100
            wrap = true
            "#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(c.preview.tab_size, 4);
        assert_eq!(c.preview.max_text_bytes, 2048);
        assert_eq!(c.preview.max_hex_bytes, 512);
        assert_eq!(c.preview.image_quality, 100);
        assert!(c.preview.wrap);
    }

    /// PLAN §3's rule at the `[preview]` table: a bad line warns with its own
    /// line number and the good ones around it still apply.
    #[test]
    fn bad_preview_lines_warn_and_keep_their_defaults() {
        let (c, warnings) = parse(
            r#"
            [preview]
            tab_size = 8
            image_quality = 300
            max_hex_bytes = "big"
            wrap = "yes"
            nonsense = 1
            "#,
        );
        assert_eq!(c.preview.tab_size, 8, "the good line applied");
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert!(
            warnings[0].message.contains("between 0 and 100"),
            "{:?}",
            warnings[0]
        );
        assert!(
            warnings[3].message.contains("unknown key"),
            "{:?}",
            warnings[3]
        );
        // Every value that failed to parse kept the shipped one.
        let d = PreviewConfig::default();
        assert_eq!(c.preview.image_quality, d.image_quality);
        assert_eq!(c.preview.max_hex_bytes, d.max_hex_bytes);
        assert_eq!(c.preview.wrap, d.wrap);
    }

    #[test]
    fn a_goto_table_replaces_the_shipped_one() {
        let (c, warnings) = parse("[goto]\nh = \"~\"\nm = \"/mnt\"\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(c.goto.len(), 2);
        assert_eq!(c.goto[1].key, "m");
        assert_eq!(c.goto[1].description, "Go to /mnt");
    }

    #[test]
    fn user_openers_and_rules_land_in_front() {
        let (c, warnings) = parse(
            r#"
            [opener.helix]
            command = 'hx "$@"'
            desc = "Helix"
            block = true

            [[open.rules]]
            mime = "text/*"
            use = ["helix"]
            "#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let openers: Vec<&str> = c
            .openers_for("a.txt", "text/plain", false)
            .into_iter()
            .map(|o| o.name.as_str())
            .collect();
        assert_eq!(openers, vec!["helix"]);
        assert!(c.opener("helix").is_some_and(|o| o.block));
        // …and the shipped rules are still underneath.
        assert!(!c.openers_for("cat.png", "image/png", false).is_empty());
    }

    #[test]
    fn a_rule_with_no_matcher_warns_and_is_dropped() {
        let (c, warnings) = parse("[[open.rules]]\nuse = [\"open\"]\n");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(c.rules, Config::default().rules);
    }

    /// `[[icon.file]]` ships empty and is the user's escape hatch: what they
    /// write is what is checked, in the order they wrote it.
    #[test]
    fn file_icon_rules_come_only_from_the_user() {
        assert!(Theme::default().file_icons.is_empty());
        let (t, warnings) = Theme::parse(
            "[[icon.file]]\nname = \"*.blend\"\ntext = \"B\"\nfg = \"#fab387\"\n\n\
             [[icon.file]]\nname = \"*\"\ntext = \"?\"\n",
            std::path::Path::new("theme.toml"),
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(t.file_icons.len(), 2);
        let blend = t.file_icon("cube.blend").expect("*.blend");
        assert_eq!(blend.text, 'B');
        assert_eq!(blend.fg, Color::parse("#fab387"));
        // First match wins, so the catch-all only answers for what the
        // specific rule did not.
        assert_eq!(t.file_icon("notes.txt").map(|i| i.text), Some('?'));
    }

    /// A bad file rule warns and the good ones still apply — PLAN §3's rule,
    /// and the same wording the directory rules produce.
    #[test]
    fn a_bad_file_icon_names_its_own_table() {
        let (t, warnings) = Theme::parse(
            "[[icon.file]]\nname = \"*.rs\"\ntext = \"too long\"\n\n\
             [[icon.file]]\nname = \"*.md\"\ntext = \"M\"\n",
            std::path::Path::new("theme.toml"),
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].message.contains("[[icon.file]]"),
            "{warnings:?}"
        );
        assert_eq!(t.file_icons.len(), 1);
        assert_eq!(t.file_icon("readme.md").map(|i| i.text), Some('M'));
    }

    #[test]
    fn theme_overrides_a_single_colour_and_prepends_icons() {
        let (t, warnings) = Theme::parse(
            "[palette]\nbase = \"#000000\"\naccent = \"#89b4fa\"\nbroken = \"nope\"\n\n[[icon.dir]]\nname = \"Work\"\ntext = \"W\"\nfg = \"#ffffff\"\n",
            Path::new("theme.toml"),
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(t.color("base"), Some(Color { r: 0, g: 0, b: 0 }));
        assert_eq!(t.color("accent"), Color::parse("#89b4fa"));
        // The rest of catppuccin survived the one override.
        assert_eq!(t.color("mauve"), Color::parse("#cba6f7"));
        let work = t.dir_icon("/x/Work", "Work").expect("Work");
        assert_eq!(work.text, 'W');
    }

    #[test]
    fn short_hex_colours() {
        assert_eq!(
            Color::parse("#fff"),
            Some(Color {
                r: 255,
                g: 255,
                b: 255
            })
        );
        assert_eq!(Color::parse("#0f0"), Some(Color { r: 0, g: 255, b: 0 }));
        assert_eq!(Color::parse("fff"), None);
        assert_eq!(Color::parse("#ffff"), None);
    }

    /// PLAN §3: a missing file is silence, not a warning.
    #[test]
    fn a_missing_config_directory_is_silently_the_defaults() {
        let loaded = load_from_dir(Path::new("/nonexistent/delightfile-test"));
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.theme, Theme::default());
    }

    #[test]
    fn bookmarks_expand_a_leading_tilde() {
        let b = Bookmark {
            key: "w".to_string(),
            path: "~/Work".to_string(),
            description: String::new(),
        };
        let expanded = b.expanded_path();
        assert!(expanded.ends_with("/Work"));
        assert!(!expanded.starts_with('~'));
        // A URL is left for the vfs to resolve.
        let b = Bookmark {
            key: "1".to_string(),
            path: "sftp://showandtour1".to_string(),
            description: String::new(),
        };
        assert_eq!(b.expanded_path(), "sftp://showandtour1");
    }
}
