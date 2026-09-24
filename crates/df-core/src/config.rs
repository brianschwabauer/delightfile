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
//! the tests assert they still match. The one place the transcription was
//! departed from on purpose is the opener rules, where a window is not a
//! terminal: see `DEFAULT_RULES` for what changed and why.
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
//! view_scale = "compact"     # compact comfortable roomy — the step every tab starts at
//! folder_sizes = true      # recursive directory sizes in the size column
//! folder_size_ttl = 600    # seconds a walked size is reused before re-walking
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
//! use = ["delightviewer", "terminal-at"]
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

/// Seconds a walked folder size is reused before the walk is run again.
///
/// Mirrors [`crate::du::DEFAULT_FOLDER_SIZE_TTL`], as a plain number because
/// this is a config file and `600` is what somebody types. The essay on why ten
/// minutes lives on the constant it mirrors.
pub const DEFAULT_FOLDER_SIZE_TTL: u64 = 600;

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
/// means delightfile waits, which only bulk-rename wants.
///
/// **`edit` is a terminal, not a wait.** yazi runs `$EDITOR` blocking because
/// yazi *is* a terminal: it hands its own tty to the editor and takes it back.
/// delightfile is a window with no tty to hand over, so a blocking `nvim` had
/// nowhere to draw and did nothing at all. It gets a terminal of its own
/// instead — `$TERMINAL`, ghostty when unset — launched the way everything
/// else here is.
///
/// `builtin:` is not a shell command: it names a job delightfile does itself.
/// The three `builtin:extract…` openers fill the gap PLAN §6 calls out — yazi
/// shells out to a plugin for this, and archives get no extract rule at all in
/// the config being ported, which is the one thing the rules were missing.
/// `extract` makes a folder named after the archive, `extract-here` spills it
/// into the directory it is in, and `extract-merged` puts several archives into
/// one folder (the picker only offers it when several are selected).
pub const DEFAULT_OPENERS: &[(&str, &str, bool, &str)] = &[
    (
        "edit",
        r#"setsid uwsm-app -- "${TERMINAL:-ghostty}" -e "${EDITOR:-vi}" "$@" >/dev/null 2>&1"#,
        false,
        "Edit in $EDITOR",
    ),
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
    // The file's counterpart to `terminal-here`: a shell in the directory the
    // file is in, which is where you want one after looking at a file.
    (
        "terminal-at",
        r#"setsid uwsm-app -- "${TERMINAL:-ghostty}" --working-directory="$(dirname "$1")" >/dev/null 2>&1"#,
        false,
        "Terminal here",
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
    ("play", r#"mpv --force-window "$@""#, false, "Play in mpv"),
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

/// Opener rules, matched top-down. Transcribed from `yazi.toml`'s
/// `[[open.prepend_rules]]`, plus the archive rules PLAN §6 asks for — and
/// then changed on purpose in three places where a window is not a terminal:
///
/// - **Text opens in Zed first.** yazi's `$EDITOR` first is a terminal
///   program's answer; from a window, the editor that is already a window is
///   the one `o` should reach. `edit` (in a terminal) and `open` (whatever
///   `xdg-open` says) follow it in the picker.
/// - **No `reveal`.** In yazi it showed the file's folder in a GUI file
///   manager, which from a file manager is a second copy of the program you
///   are already in. `terminal-at` — a shell in that folder — is the thing
///   you actually leave for, so it is the last entry of every file rule
///   instead, bar bulk-rename's and the archives' (`extract`, then `open`). A
///   rule that `reveal` was the only alternative in gets `open`, so `O` still
///   offers the system default.
/// - **`edit` runs in a terminal** (see [`DEFAULT_OPENERS`]).
///
/// The by-name rules come first for the reason the yazi config gives: an `.obj`
/// and a `.ply` are `text/plain` and an `.stl` is `application/octet-stream`,
/// so a mime rule would send half the 3D formats to an editor and half to
/// nothing. A `.gcode` is `text/plain` to every detector on the machine, which
/// would open a 40 MB toolpath in a text editor.
const DEFAULT_RULES: &[(&str, &str, &[&str])] = &[
    ("glob", "bulk-rename.txt", &["bulk-rename"]),
    (
        "glob",
        "*.{stl,obj,ply,3mf}",
        &["delightviewer", "open", "terminal-at"],
    ),
    (
        "glob",
        "*.{gcode,gco}",
        &["delightviewer", "edit", "open", "terminal-at"],
    ),
    (
        "glob",
        "*.{ttf,otf,ttc}",
        &["delightviewer", "open", "terminal-at"],
    ),
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
    (
        "mime",
        "image/*",
        &[
            "delightviewer",
            "delightviewer-edit",
            "optimize-avif",
            "set-wallpaper",
            "edit-image",
            "terminal-at",
        ],
    ),
    (
        "mime",
        "video/*",
        &["delightviewer", "delightviewer-edit", "play", "terminal-at"],
    ),
    (
        "mime",
        "audio/*",
        &["delightviewer", "delightviewer-edit", "terminal-at"],
    ),
    (
        "mime",
        "application/pdf",
        &["delightviewer", "delightviewer-edit", "terminal-at"],
    ),
    ("glob", "*/", &["open", "zed-workspace", "terminal-here"]),
    // The fallback. yazi leaves this implicit; writing it down means the picker
    // is never empty, which is the one state `O` must not have.
    ("glob", "*", &["open", "terminal-at"]),
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

/// How big the list pane draws itself — Windows Explorer's view slider, as an
/// ordered ladder rather than a continuous zoom.
///
/// The four steps are one axis, which is the whole point: `-` walks down it and
/// `=` walks up it, and the top of the ladder **is** the grid. A separate "list
/// size" setting beside a separate "grid on/off" toggle would be two controls
/// for one question — how much room does a file get — and the person dragging
/// Explorer's slider from Details to Extra Large Icons is not thinking about
/// two of anything.
///
/// A step scales the row's *content*: its height, its icon and its text, in one
/// ratio so the row keeps its proportions. Nothing else moves. The top bar, the
/// pane widths and the rest of the chrome are the window's furniture rather
/// than the listing's, and a step that grew the tab strip with it would be a
/// zoom, which this deliberately is not.
///
/// Declaration order is the ladder's order, and [`Ord`] is derived from it, so
/// the step functions and the tests are comparisons rather than match arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub enum ViewScale {
    /// Today's list: 22 pt rows at a 13.5 pt face, the density yazi's muscle
    /// memory expects. The smallest step, and the default, because the program
    /// is a port of that muscle memory before it is anything else.
    #[default]
    Compact,
    /// A fifth taller. The step somebody reaches for on a 4K panel across a
    /// desk, where compact is honest and small.
    Comfortable,
    /// The largest the *list* goes: names at nearly half again, icons big
    /// enough to tell a folder from a film at a glance down the column.
    Roomy,
    /// The thumbnail grid — the top of the ladder, not a mode beside it.
    Grid,
}

/// The steps, in order. One array, so the ladder is written down once and the
/// step functions, the config parser and the tests all read the same one.
pub const VIEW_SCALES: [ViewScale; 4] = [
    ViewScale::Compact,
    ViewScale::Comfortable,
    ViewScale::Roomy,
    ViewScale::Grid,
];

impl ViewScale {
    pub fn from_name(name: &str) -> Option<ViewScale> {
        Some(match name {
            "compact" => ViewScale::Compact,
            "comfortable" => ViewScale::Comfortable,
            "roomy" => ViewScale::Roomy,
            "grid" => ViewScale::Grid,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            ViewScale::Compact => "compact",
            ViewScale::Comfortable => "comfortable",
            ViewScale::Roomy => "roomy",
            ViewScale::Grid => "grid",
        }
    }

    /// The next step up, or `None` at the top.
    ///
    /// `None` rather than a saturating step, so the caller can tell "already at
    /// the largest" from "moved" and skip the write and the toast that go with
    /// a change — a key that says "Grid view" every time you press it at the
    /// top of the ladder is a key that is lying about having done something.
    pub fn larger(self) -> Option<ViewScale> {
        let at = VIEW_SCALES.iter().position(|s| *s == self)?;
        VIEW_SCALES.get(at + 1).copied()
    }

    /// The next step down, or `None` at the bottom.
    pub fn smaller(self) -> Option<ViewScale> {
        let at = VIEW_SCALES.iter().position(|s| *s == self)?;
        at.checked_sub(1).and_then(|i| VIEW_SCALES.get(i).copied())
    }

    /// Whether this step draws the pane as a wall of tiles.
    pub fn is_grid(self) -> bool {
        self == ViewScale::Grid
    }

    /// The largest step that is still a list.
    ///
    /// What the grid falls back to for anything still measured in rows: the
    /// parent column beside a grid, and the step `-` lands on coming down out
    /// of one.
    pub const fn largest_list() -> ViewScale {
        ViewScale::Roomy
    }

    /// This step as a *list* step: itself, or [`ViewScale::largest_list`] for
    /// the grid.
    pub fn as_list(self) -> ViewScale {
        if self.is_grid() {
            ViewScale::largest_list()
        } else {
            self
        }
    }

    /// How far a row's height, icon and text are multiplied at this step.
    ///
    /// 1, 1.2, 1.45 — a ratio a shade under a fifth each time, which is the
    /// smallest step that reads as a *different size* rather than as a
    /// rendering wobble, and small enough that three of them do not turn a
    /// screenful into six rows. The grid reports the largest list step's
    /// number, because the only thing still measured in rows beside a grid is
    /// a list (see [`ViewScale::as_list`]).
    pub fn row_factor(self) -> f32 {
        match self.as_list() {
            ViewScale::Comfortable => 1.2,
            ViewScale::Roomy => 1.45,
            // `Compact`; the grid never reaches here.
            _ => 1.0,
        }
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
    /// The step on the [`ViewScale`] ladder every tab starts at.
    ///
    /// Every new tab, and every new window — which is a new process — opens at
    /// this step. From there `-`, `=` and `Ctrl+g` move only the tab they were
    /// pressed in, and it keeps its step wherever it goes for the rest of the
    /// session; nothing about it is remembered per directory or across a
    /// restart (it was until 2026-09-23).
    ///
    /// A *list* step only: `grid` parses as a name but is refused as this
    /// default, with a warning, as it has been since the ladder was added.
    pub view_scale: ViewScale,
    /// Whether directories get a recursive size in the size column (PLAN §7.3).
    ///
    /// On, because a size column that says nothing for half its rows is a
    /// column you stop reading. Off is for the machine where `~` is an NFS
    /// mount and a background walk is a bill somebody pays — the flag exists so
    /// that person does not have to choose between the size column and their
    /// network.
    pub folder_sizes: bool,
    /// How long a walked folder size is served before it is earned again, in
    /// seconds (PLAN §7.3).
    ///
    /// The number that decides whether leaving a folder and coming back costs
    /// anything. Inside it, a revisit draws the remembered totals and starts no
    /// walk; outside it, the numbers are still drawn — wearing their `~` — and a
    /// walk corrects them behind. Ten minutes is a browsing session's worth of
    /// in-and-out.
    ///
    /// `0` turns the reuse off and makes every revisit a fresh walk, which is
    /// the old behaviour and is only right on a directory somebody else is
    /// writing to continuously.
    pub folder_size_ttl: u64,
    /// The file-type filter every listing is narrowed to, when this session is
    /// a file dialog that offered some (see [`crate::fs::TypeFilter`]).
    ///
    /// **Not a key in `[mgr]`.** No file sets it; the picker does, beside the
    /// `show_hidden` it overrides, and for the same reason it lives here: this
    /// struct is what every new listing is built from, so a directory entered
    /// mid-session is narrowed from its first batch like it is hidden-filtered
    /// from its first batch.
    pub types: Option<crate::fs::TypeFilter>,
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
            view_scale: ViewScale::Compact,
            folder_sizes: true,
            folder_size_ttl: DEFAULT_FOLDER_SIZE_TTL,
            types: None,
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
    Mime(Glob),
    /// Matched against the file name, or the full path when the pattern
    /// contains a `/`. A trailing `/` means "a directory" (`*/`).
    Name(Glob),
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
                        Matcher::Mime(Glob::new(*pattern))
                    } else {
                        Matcher::Name(Glob::new(*pattern))
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
                Matcher::Mime(pattern) => !is_dir && pattern.matches(mime),
                Matcher::Name(pattern) => pattern.matches(&dir_name),
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
                    // `grid` is a step of the ladder but not a legal default —
                    // see [`MgrConfig::view_scale`]. Warned about rather than
                    // silently dropped, so somebody who tried it learns that
                    // it did not take.
                    "view_scale" => read_enum(
                        value,
                        |name| ViewScale::from_name(name).filter(|s| !s.is_grid()),
                        &mut config.mgr.view_scale,
                    ),
                    "folder_sizes" => read_bool(value, &mut config.mgr.folder_sizes),
                    "folder_size_ttl" => {
                        let mut seconds = config.mgr.folder_size_ttl as usize;
                        let r = read_usize(value, &mut seconds);
                        config.mgr.folder_size_ttl = seconds as u64;
                        r
                    }
                    _ => Err(format!("unknown key `{}` in [mgr]", entry.key)),
                };
                if let Err(message) = ok {
                    warnings.push(ConfigWarning::new(file, entry.line, message));
                }
            }
        }

        // `[input]` has no settings left. It is still read so that a config
        // carried over from the modal era says *why* it stopped doing
        // anything, rather than being reported as a table full of typos.
        if let Some(input) = doc.table("input") {
            for entry in &input.entries {
                let message = if entry.key == "vi_mode" {
                    "`vi_mode` is gone: prompts are never modal, and `Esc` \
                     always cancels"
                        .to_string()
                } else {
                    format!("unknown key `{}` in [input]", entry.key)
                };
                warnings.push(ConfigWarning::new(file, entry.line, message));
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
            let mut said = Vec::new();
            match parse_rule(table, &mut said) {
                Ok(rule) => user_rules.push(rule),
                Err(message) => said.push(message),
            }
            for message in said {
                warnings.push(ConfigWarning::new(file, table.line, message));
            }
        }
        if !user_rules.is_empty() {
            user_rules.append(&mut config.rules);
            config.rules = user_rules;
        }

        (config, warnings)
    }
}

/// One `[[open.rules]]`. `said` collects the things that are worth a warning
/// but are not a reason to throw the rule away — a brace expansion that had to
/// stop, most of all.
fn parse_rule(table: &Table, said: &mut Vec<String>) -> Result<OpenRule, String> {
    let mut compile = |pattern: &str| {
        let (glob, warning) = Glob::compile(pattern);
        said.extend(warning);
        glob
    };
    let matcher = match (
        table.get("mime").and_then(Value::as_str),
        table.get("glob").and_then(Value::as_str),
    ) {
        (Some(mime), None) => Matcher::Mime(compile(mime)),
        (None, Some(glob)) => Matcher::Name(compile(glob)),
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
    pub pattern: Glob,
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
    pub pattern: Glob,
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
                    pattern: Glob::new(*pattern),
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
            let subject = if i.pattern.as_str().contains('/') {
                path
            } else {
                name
            };
            i.pattern.matches(subject)
        })
    }

    /// The user's icon rule for a file, first rule wins, or `None` to fall
    /// through to df-app's per-kind table.
    pub fn file_icon(&self, name: &str) -> Option<&FileIcon> {
        self.file_icons.iter().find(|i| i.pattern.matches(name))
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
            let mut said = Vec::new();
            match parse_dir_icon(table, &mut said) {
                Ok(icon) => icons.push(icon),
                Err(message) => said.push(message),
            }
            for message in said {
                warnings.push(ConfigWarning::new(file, table.line, message));
            }
        }
        if !icons.is_empty() {
            icons.append(&mut theme.dir_icons);
            theme.dir_icons = icons;
        }

        // File rules ship empty, so there is nothing to prepend to: the list is
        // the user's, in the order they wrote it, and first match still wins.
        for table in doc.tables_named("icon.file") {
            let mut said = Vec::new();
            match parse_file_icon(table, &mut said) {
                Ok(icon) => theme.file_icons.push(icon),
                Err(message) => said.push(message),
            }
            for message in said {
                warnings.push(ConfigWarning::new(file, table.line, message));
            }
        }

        (theme, warnings)
    }
}

fn parse_dir_icon(table: &Table, said: &mut Vec<String>) -> Result<DirIcon, String> {
    let (pattern, text, fg) = parse_icon_fields(table, "icon.dir", said)?;
    Ok(DirIcon { pattern, text, fg })
}

fn parse_file_icon(table: &Table, said: &mut Vec<String>) -> Result<FileIcon, String> {
    let (pattern, text, fg) = parse_icon_fields(table, "icon.file", said)?;
    // A file rule is matched against the *name*, never the path (see
    // [`Theme::file_icon`]), so a `/` in the pattern is a rule that can never
    // fire. That is almost always somebody reaching for the directory rule's
    // path matching, so say which one they wanted.
    if pattern.as_str().contains('/') {
        said.push(format!(
            "[[icon.file]] `name = \"{}\"` can never match — file rules are matched against the name alone; [[icon.dir]] is the one that takes a path",
            pattern.as_str()
        ));
    }
    Ok(FileIcon { pattern, text, fg })
}

/// The keys an icon rule may have. Anything else is a typo, and a typo that is
/// quietly ignored is a rule that does not do what it says.
const ICON_KEYS: &[&str] = &["name", "text", "fg"];

/// The three fields both icon rules share, validated once. `what` is the table
/// name without its brackets, so a warning can name either spelling of it.
///
/// `said` takes the complaints that do not sink the rule — an unknown key, a
/// brace expansion that had to stop, the single-bracket header — because a
/// rule with a typo beside three good fields is still a rule the user wants.
fn parse_icon_fields(
    table: &Table,
    what: &str,
    said: &mut Vec<String>,
) -> Result<(Glob, char, Option<Color>), String> {
    // `[icon.file]` parses into the same `Table` as `[[icon.file]]` and would
    // work exactly once, then silently swallow every rule after it — TOML only
    // allows one table of a given name. Warn and keep going: the rule the user
    // wrote is the rule they meant.
    if !table.array_element {
        said.push(format!(
            "`[{what}]` is a single table — icon rules are a list, so write `[[{what}]]`"
        ));
    }
    for entry in &table.entries {
        if !ICON_KEYS.contains(&entry.key.as_str()) {
            said.push(format!("unknown key `{}` in [[{what}]]", entry.key));
        }
    }
    let pattern = table
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("[[{what}]] has no `name`"))?;
    let text = table
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("[[{what}]] has no `text`"))?;
    let mut chars = text.chars();
    let (Some(glyph), None) = (chars.next(), chars.next()) else {
        return Err(format!(
            "[[{what}]] `text = \"{text}\"` must be one character"
        ));
    };
    let fg = match table.get("fg") {
        Some(value) => match value.as_str().and_then(Color::parse) {
            Some(color) => Some(color),
            None => return Err(format!("[[{what}]] `fg` must be a colour like \"#89b4fa\"")),
        },
        None => None,
    };
    // An empty pattern matches nothing at all, which is the other way to write
    // a rule that never fires.
    if pattern.is_empty() {
        said.push(format!("[[{what}]] `name` is empty — it can never match"));
    }
    let (pattern, warning) = Glob::compile(pattern);
    said.extend(warning);
    Ok((pattern, glyph, fg))
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

/// The most alternatives one `{a,b,c}` pattern may expand to.
///
/// Brace expansion multiplies: `{a,b}{c,d}{e,f}…` doubles per group, so a
/// pattern a person can type in half a line can name millions of strings. Sixty
/// four is well past every rule in the shipped tables (the biggest, yazi's
/// 3D-model list, is twenty) and small enough that the worst case is a shrug
/// rather than a hang. Past it the pattern stops expanding and is matched with
/// its braces intact — and the reader is told, because a rule that quietly
/// stopped meaning what it says is worse than one that never fired.
const MAX_ALTERNATIVES: usize = 64;

/// A glob, expanded and lowercased once.
///
/// `*`, `?` and `{a,b,c}` alternatives, matched case-insensitively.
///
/// Case-insensitive on purpose: the yazi config this is ported from spells
/// every extension twice (`{stl,STL,obj,OBJ,…}`) because its matcher is not,
/// and a rule that opens `photo.JPG` in a text editor is not a rule anybody
/// wanted. Mime types are matched the same way — `image/*` and
/// `application/{json,xml}` are the same shape of pattern.
///
/// **The work happens at parse time.** Lowercasing the pattern and expanding
/// its braces used to happen inside every call, so a directory of 200k rows
/// re-expanded the same twenty alternatives per row per rule — and the
/// expansion is exponential, so the cost of a pattern was paid over and over
/// instead of once. Here it is paid when the rule is read, and matching is a
/// byte walk over strings that are already in the right case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    /// As the user wrote it, for warnings and for the `/` test that decides
    /// whether a directory rule addresses a path or a name.
    pattern: String,
    /// Lowercased, braces expanded. Never empty: a pattern with nothing to
    /// expand is its own single alternative.
    alternatives: Vec<String>,
}

impl Glob {
    pub fn new(pattern: impl Into<String>) -> Glob {
        Glob::compile(pattern).0
    }

    /// Compile, and say so when the brace expansion hit [`MAX_ALTERNATIVES`].
    pub fn compile(pattern: impl Into<String>) -> (Glob, Option<String>) {
        let pattern = pattern.into();
        let (alternatives, truncated) = expand_braces(&pattern.to_ascii_lowercase());
        let warning = truncated.then(|| {
            format!(
                "`{pattern}` expands past {MAX_ALTERNATIVES} alternatives — the rest of its braces are matched literally"
            )
        });
        (
            Glob {
                pattern,
                alternatives,
            },
            warning,
        )
    }

    /// The pattern as written.
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Does `text` match? `text` is folded a byte at a time rather than
    /// lowercased into a new `String`, because this is the per-row call.
    pub fn matches(&self, text: &str) -> bool {
        self.alternatives
            .iter()
            .any(|alt| wildcard_match(alt.as_bytes(), text.as_bytes()))
    }
}

/// `*`, `?` and `{a,b,c}`, compiled on the spot. For one-off matches and tests;
/// anything matched once per row keeps a [`Glob`] instead.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    Glob::new(pattern).matches(text)
}

/// `a{b,c}d` → `["abd", "acd"]`, breadth-first so the queue's length is the
/// running count and the cap can be applied before the work is done rather
/// than after it. The `bool` is "this hit [`MAX_ALTERNATIVES`]".
fn expand_braces(pattern: &str) -> (Vec<String>, bool) {
    let mut done: Vec<String> = Vec::new();
    let mut queue: Vec<String> = vec![pattern.to_string()];
    let mut at = 0usize;
    let mut truncated = false;
    while at < queue.len() {
        let current = std::mem::take(&mut queue[at]);
        at += 1;
        let Some((head, pieces, tail)) = split_first_brace(&current) else {
            done.push(current);
            continue;
        };
        // Everything finished, everything still queued, and what this group is
        // about to add. Over the line, the pattern keeps its braces and is
        // matched as the literal text it is.
        if done.len() + (queue.len() - at) + pieces.len() > MAX_ALTERNATIVES {
            truncated = true;
            done.push(current);
            continue;
        }
        for piece in pieces {
            queue.push(format!("{head}{piece}{tail}"));
        }
    }
    (done, truncated)
}

/// The first balanced `{…}` group: what is before it, its comma-separated
/// pieces, and what is after. `None` when there is no group to expand —
/// including an unbalanced brace, which is a literal brace and not an error: a
/// file really can be called `{draft}.txt`.
fn split_first_brace(pattern: &str) -> Option<(String, Vec<String>, String)> {
    let open = pattern.find('{')?;
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
    let close = close?;
    let mut pieces = Vec::new();
    let mut depth = 0i32;
    let mut start = open + 1;
    for (i, c) in pattern[open + 1..close].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                pieces.push(pattern[start..open + 1 + i].to_string());
                start = open + 1 + i + 1;
            }
            _ => {}
        }
    }
    pieces.push(pattern[start..close].to_string());
    Some((
        pattern[..open].to_string(),
        pieces,
        pattern[close + 1..].to_string(),
    ))
}

/// `*` (any run) and `?` (one character), iteratively with backtracking — no
/// recursion, so a pathological pattern cannot blow the stack. `pattern` is
/// already lowercase; `text` is folded as it is walked.
fn wildcard_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t].to_ascii_lowercase()) {
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

    /// The ladder is an *order*, and `-`/`=` are a walk along it: from the
    /// bottom, three steps up reach the grid and nothing is skipped.
    #[test]
    fn the_view_scale_ladder_climbs_compact_to_grid_one_step_at_a_time() {
        let mut at = ViewScale::Compact;
        let mut walked = vec![at];
        while let Some(next) = at.larger() {
            at = next;
            walked.push(at);
        }
        assert_eq!(walked, VIEW_SCALES.to_vec());
        assert_eq!(at, ViewScale::Grid);
        // …and back down the same rungs, in the same order.
        let mut back = vec![at];
        while let Some(prev) = at.smaller() {
            at = prev;
            back.push(at);
        }
        back.reverse();
        assert_eq!(back, VIEW_SCALES.to_vec());
        // The ends are ends: `-` at the bottom and `=` at the top do nothing.
        assert_eq!(ViewScale::Compact.smaller(), None);
        assert_eq!(ViewScale::Grid.larger(), None);
        // Declaration order is the ladder's order, so `Ord` agrees with it.
        assert!(ViewScale::Compact < ViewScale::Comfortable);
        assert!(ViewScale::Roomy < ViewScale::Grid);
    }

    /// The grid transition, from both sides: `=` off the largest list step
    /// lands in the tiles, and `-` out of the tiles lands back on that same
    /// step rather than on wherever the list happened to be before.
    #[test]
    fn the_top_of_the_ladder_is_the_grid() {
        assert_eq!(ViewScale::largest_list(), ViewScale::Roomy);
        assert_eq!(ViewScale::Roomy.larger(), Some(ViewScale::Grid));
        assert_eq!(ViewScale::Grid.smaller(), Some(ViewScale::largest_list()));
        assert!(ViewScale::Grid.is_grid());
        assert!(!ViewScale::Roomy.is_grid());
        // Anything measured in rows beside a grid — the parent column — reads
        // the grid as the largest list step rather than as a hole.
        assert_eq!(ViewScale::Grid.as_list(), ViewScale::Roomy);
        assert_eq!(ViewScale::Grid.row_factor(), ViewScale::Roomy.row_factor());
    }

    /// Each step is bigger than the one below it, and the bottom one is
    /// today's list untouched — a scale that moved the default row height
    /// would be a redesign wearing a feature's name.
    #[test]
    fn every_step_is_larger_than_the_last_and_the_smallest_changes_nothing() {
        assert_eq!(ViewScale::Compact.row_factor(), 1.0);
        for pair in VIEW_SCALES.windows(2) {
            let [small, large] = pair else { continue };
            assert!(
                large.row_factor() >= small.row_factor(),
                "{small:?} → {large:?}"
            );
        }
        assert!(ViewScale::Roomy.row_factor() > ViewScale::Comfortable.row_factor());
    }

    /// Every step round-trips through its name, which is what the config
    /// stores.
    #[test]
    fn view_scale_names_round_trip() {
        for scale in VIEW_SCALES {
            assert_eq!(ViewScale::from_name(scale.name()), Some(scale));
        }
        assert_eq!(ViewScale::from_name("huge"), None);
    }

    /// `[mgr] view_scale` names a list step. `grid` is a step of the ladder and
    /// still not a legal default — see [`MgrConfig::view_scale`].
    #[test]
    fn the_view_scale_default_is_a_list_step() {
        assert_eq!(Config::default().mgr.view_scale, ViewScale::Compact);
        let (config, warnings) = parse("[mgr]\nview_scale = \"roomy\"\n");
        assert_eq!(config.mgr.view_scale, ViewScale::Roomy);
        assert!(warnings.is_empty(), "{warnings:?}");
        let (config, warnings) = parse("[mgr]\nview_scale = \"grid\"\n");
        assert_eq!(config.mgr.view_scale, ViewScale::Compact);
        assert_eq!(warnings.len(), 1);
    }

    /// `[input]` is an empty table now, and a config that still sets
    /// `vi_mode` is told why nothing happened rather than being left to
    /// wonder — a silent no-op is how a setting becomes a bug report.
    #[test]
    fn the_retired_vi_mode_switch_warns() {
        let (_, warnings) = parse("[input]\nvi_mode = true\n");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("vi_mode"), "{warnings:?}");
        // A typo in the same table warns like every other table's does.
        let (_, warnings) = parse("[input]\nvi = true\n");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("unknown key"), "{warnings:?}");
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
        // Text: Zed first, then the terminal editor, then the system default,
        // and a shell in the file's folder last.
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
        // Glob, ahead of the mime rules on purpose: a .obj is text/plain.
        assert_eq!(
            names(c.openers_for("bracket.OBJ", "text/plain", false)),
            vec!["delightviewer", "open", "terminal-at"]
        );
        // The toolpath and font rules end in the same shell as every other
        // file rule. A `.gcode` is `text/plain`, which is why its rule is
        // matched by name, ahead of the text rule.
        assert_eq!(
            names(c.openers_for("benchy.gcode", "text/plain", false)),
            vec!["delightviewer", "edit", "open", "terminal-at"]
        );
        assert_eq!(
            names(c.openers_for("Inter.ttf", "font/ttf", false)),
            vec!["delightviewer", "open", "terminal-at"]
        );
        // Media and PDFs end in the same shell.
        for (name, mime) in [
            ("cat.png", "image/png"),
            ("clip.mp4", "video/mp4"),
            ("song.mp3", "audio/mpeg"),
            ("paper.pdf", "application/pdf"),
        ] {
            let got = names(c.openers_for(name, mime, false));
            assert_eq!(got.first().map(String::as_str), Some("delightviewer"));
            assert_eq!(
                got.last().map(String::as_str),
                Some("terminal-at"),
                "{name}"
            );
        }
        // The archive rule PLAN §6 asks for, which yazi's config was missing.
        let extract = c.openers_for("backup.tar.gz", "application/gzip", false);
        assert_eq!(
            names(extract),
            vec!["extract", "extract-here", "extract-merged", "open"]
        );
        // A 7z's mime is enough on its own, whatever it is called.
        assert_eq!(
            names(c.openers_for("backup", "application/x-7z-compressed", false))[0],
            "extract"
        );
        for id in ["extract", "extract-here", "extract-merged"] {
            assert_eq!(c.opener(id).and_then(Opener::builtin), Some(id));
        }
        // Directories match `*/`, whatever their mime.
        assert_eq!(
            names(c.openers_for("Work", "inode/directory", true)),
            vec!["open", "zed-workspace", "terminal-here"]
        );
        // …and nothing is ever an empty picker.
        assert_eq!(
            names(c.openers_for("mystery", "application/octet-stream", false)),
            vec!["open", "terminal-at"]
        );
    }

    /// `edit` used to wait on `$EDITOR` with no terminal behind it, which for
    /// a terminal editor is nothing happening at all; and `reveal` opened a
    /// second file manager from inside this one. Both are gone for good.
    #[test]
    fn edit_opens_a_terminal_and_nothing_reveals() {
        let c = Config::default();
        let edit = c.opener("edit").expect("edit");
        assert!(!edit.block, "a blocking editor from a window has no tty");
        assert!(edit.command.contains("${TERMINAL:-ghostty}"));
        assert!(edit.command.contains("${EDITOR:-vi}"));
        assert_eq!(edit.description, "Edit in $EDITOR");

        let terminal = c.opener("terminal-at").expect("terminal-at");
        assert!(!terminal.block);
        assert!(terminal
            .command
            .contains(r#"--working-directory="$(dirname "$1")""#));

        assert!(c.opener("reveal").is_none(), "the opener is gone");
        for rule in &c.rules {
            assert!(
                !rule.openers.iter().any(|name| name == "reveal"),
                "{rule:?} still names reveal"
            );
            // A rule naming an opener that does not exist drops it silently
            // (`openers_for`), so a leftover would be a quiet hole in `O`.
            for name in &rule.openers {
                assert!(c.opener(name).is_some(), "{rule:?} names a missing {name}");
            }
        }
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

    /// Everything an icon rule can get wrong that is not fatal to it: a typo
    /// for a key, the single-bracket header, and a pattern that can never fire.
    #[test]
    fn icon_rules_warn_about_what_they_quietly_ignored() {
        let (t, warnings) = Theme::parse(
            "[[icon.file]]\nname = \"*.rs\"\ntext = \"R\"\ncolour = \"#ff0000\"\n",
            std::path::Path::new("theme.toml"),
        );
        assert_eq!(t.file_icons.len(), 1, "the rule itself is still good");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].message.contains("colour"), "{warnings:?}");

        let (t, warnings) = Theme::parse(
            "[icon.file]\nname = \"*.rs\"\ntext = \"R\"\n",
            std::path::Path::new("theme.toml"),
        );
        assert_eq!(t.file_icons.len(), 1);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].message.contains("[[icon.file]]"),
            "{warnings:?}"
        );

        // `/` never appears in a file name, so this rule is dead on arrival.
        let (_, warnings) = Theme::parse(
            "[[icon.file]]\nname = \"/mnt/*/plex\"\ntext = \"P\"\n",
            std::path::Path::new("theme.toml"),
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].message.contains("can never match"),
            "{warnings:?}"
        );

        // …but a directory rule may address a path, and must not warn.
        let (_, warnings) = Theme::parse(
            "[[icon.dir]]\nname = \"/mnt/*/plex\"\ntext = \"P\"\n",
            std::path::Path::new("theme.toml"),
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// A pattern that would expand into millions of alternatives stops, says
    /// so, and matches what is left literally rather than hanging the parse.
    #[test]
    fn brace_expansion_is_capped_and_says_so() {
        let wide: String = std::iter::repeat_n("{a,b}", 20).collect();
        let (glob, warning) = Glob::compile(format!("*{wide}"));
        assert!(warning.is_some(), "a 2^20 pattern must warn");
        assert!(glob.matches("*aaaaaaaaaaaaaaaaaaaa") || !glob.matches("anything"));

        // The rules people actually write are nowhere near the cap.
        let (glob, warning) = Glob::compile("*.{png,jpg,jpeg,gif,webp,avif,heic}");
        assert!(warning.is_none());
        assert!(glob.matches("holiday.HEIC"));
        assert!(!glob.matches("holiday.raw"));

        let (_, warning) = Theme::parse(
            &format!("[[icon.file]]\nname = \"*{wide}\"\ntext = \"X\"\n"),
            std::path::Path::new("theme.toml"),
        );
        assert_eq!(warning.len(), 1, "{warning:?}");
        assert!(warning[0].message.contains("alternatives"), "{warning:?}");
    }

    /// The expansion is done once, at parse time, and the alternatives are
    /// already lowercase — so a match is a byte walk and nothing else.
    #[test]
    fn a_glob_expands_once_and_keeps_its_pattern() {
        let glob = Glob::new("*.{RS,Md}");
        assert_eq!(glob.as_str(), "*.{RS,Md}", "the warning needs the original");
        assert_eq!(glob.alternatives, vec!["*.rs", "*.md"]);
        assert!(glob.matches("LIB.RS"));
        assert!(glob.matches("lib.rs"));
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
