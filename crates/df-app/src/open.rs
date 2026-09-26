//! Opening things, and the `;` / `:` shell (PLAN §4.1, §6).
//!
//! Two jobs, one mechanism. An opener rule's `command` and a line typed at the
//! `;` prompt are both **shell snippets with positional parameters** — yazi's
//! `$1` and `$@` — so both are run the same way:
//!
//! ```text
//! $SHELL -c '<snippet>' delightfile <path> <path> …
//! ```
//!
//! The paths go past the shell as *arguments*, never spliced into the string.
//! That is the whole safety story: a file called `; rm -rf ~` is one argument
//! with a semicolon in its name, and no amount of quoting cleverness has to be
//! got right for it to stay that way. `delightfile` is `$0`, which is what the
//! shell puts in an error message, so a failing snippet says which program ran
//! it.
//!
//! Detaching (PLAN §6's "launched detached") is `setsid`: the opener commands
//! ported from the yazi config already say `setsid uwsm-app --` themselves, and
//! a bare `;` command should be no less free of delightfile's process group —
//! quit the file manager and what you started stays up. It is a real program
//! rather than a `pre_exec` closure because the workspace lints
//! `unsafe_code = "warn"`, and this does not need unsafe to be correct.

use std::path::{Path, PathBuf};
use std::process::Command;

use df_core::config::{Config, Opener};
use df_core::fs::Entry;

use crate::chrome::{self, CARD_PAD, FONT, PAD_X};
use crate::hover::{pressed_rect, Hovers};
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting};

/// `$0` for every snippet delightfile runs.
const ARGV0: &str = "delightfile";

/// The shell, in `$SHELL` order of preference. POSIX `sh` is the fallback
/// because it is the one binary that is definitely there.
pub fn shell_program() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

/// The full argv for running `snippet` over `paths`.
///
/// Pure, and the reason it is: the argument order *is* the contract with every
/// opener rule in the shipped config (`"$@"`, `"$1"`), and getting `$0` wrong
/// would silently shift every one of them by one.
pub fn shell_argv(shell: &str, snippet: &str, paths: &[PathBuf]) -> Vec<String> {
    let mut argv = vec![
        shell.to_string(),
        "-c".to_string(),
        snippet.to_string(),
        ARGV0.to_string(),
    ];
    argv.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
    argv
}

/// Wrap an argv so the child leaves delightfile's process group.
///
/// `--fork` is not optional: without it `setsid` *execs* the program in place
/// whenever the caller is not already a process-group leader, and delightfile
/// usually is not — so the "detached" child would be this very process's child
/// and the `wait` in [`spawn_detached`] would block the UI thread for as long
/// as the editor stayed open. With `--fork` the direct child exits immediately
/// and the grandchild is the compositor's.
///
/// `setsid` is util-linux and is on every machine this targets; if it is
/// somehow not there the command still runs — just parented to us, which is
/// worse than detached and much better than not opening the file.
pub fn detached_argv(argv: Vec<String>) -> Vec<String> {
    if which("setsid").is_none() {
        return argv;
    }
    let mut out = vec!["setsid".to_string(), "--fork".to_string()];
    out.extend(argv);
    out
}

/// Is `name` on `$PATH`?
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn command_from(argv: &[String], cwd: &Path) -> Option<Command> {
    let (program, args) = argv.split_first()?;
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd);
    Some(command)
}

/// Start `snippet` and do not wait for it (`;`, and every non-blocking opener).
pub fn spawn_detached(snippet: &str, paths: &[PathBuf], cwd: &Path) -> std::io::Result<()> {
    let plain = shell_argv(&shell_program(), snippet, paths);
    let argv = detached_argv(plain.clone());
    let detached = argv.len() != plain.len();
    let mut command =
        command_from(&argv, cwd).ok_or_else(|| std::io::Error::other("empty command"))?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn()?;
    if detached {
        // `setsid --fork` is gone the instant it has forked, so this reaps a
        // process that has already exited rather than waiting on the editor.
        let _ = child.wait();
    }
    Ok(())
}

/// Run `snippet` to completion (`:`, and `block = true` openers). Called on a
/// task worker, never on the UI thread.
pub fn run_blocking(snippet: &str, paths: &[PathBuf], cwd: &Path) -> std::io::Result<i32> {
    let argv = shell_argv(&shell_program(), snippet, paths);
    let mut command =
        command_from(&argv, cwd).ok_or_else(|| std::io::Error::other("empty command"))?;
    let status = command.status()?;
    // A signalled child has no code; 128 + signal is what every shell reports
    // for one, so the toast says the number the user would see in `$?`.
    Ok(status.code().unwrap_or_else(|| {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    }))
}

/// How a finished blocking command reads in a toast.
pub fn exit_text(snippet: &str, code: i32) -> String {
    let snippet = short(snippet);
    if code == 0 {
        format!("{snippet} — done")
    } else {
        format!("{snippet} — exit {code}")
    }
}

/// A snippet, cut to something that fits on one line of chrome.
pub fn short(snippet: &str) -> String {
    const MAX: usize = 42;
    let one_line: String = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= MAX {
        return one_line;
    }
    let cut: String = one_line.chars().take(MAX - 1).collect();
    format!("{cut}…")
}

// ── Opener rules (PLAN §6) ──────────────────────────────────────────────────

/// One way to open the file under the cursor, already detached from the config
/// so the picker can outlive the borrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub name: String,
    pub command: String,
    pub description: String,
    pub block: bool,
}

impl Choice {
    fn from(opener: &Opener) -> Choice {
        Choice {
            name: opener.name.clone(),
            command: opener.command.clone(),
            description: opener.description.clone(),
            block: opener.block,
        }
    }

    /// The job name when this is something delightfile does itself rather than
    /// a shell command (`builtin:extract`).
    pub fn builtin(&self) -> Option<&str> {
        self.command.strip_prefix("builtin:")
    }
}

/// Every opener that matches `entry`, in picker order — the first is what plain
/// `o` runs.
///
/// A piece of a multi-part archive is matched as the archive it is a piece of:
/// `backup.7z.002` has no extension a rule could name and no bytes a sniffer
/// could recognise, but `o` on it should extract `backup.7z` like `o` on the
/// first piece does, so the rules are asked about `backup.7z`.
pub fn choices_for(config: &Config, entry: &Entry) -> Vec<Choice> {
    let whole = (!entry.is_dir())
        .then(|| df_core::archive::volume_of(&entry.name))
        .flatten()
        .map(|volume| volume.whole_name());
    config
        .openers_for(
            whole.as_deref().unwrap_or(&entry.name),
            entry.mime,
            entry.is_dir(),
        )
        .into_iter()
        .map(Choice::from)
        .collect()
}

/// The opener "Open terminal here" runs: the one the shipped `*/` rule offers
/// on a folder's row, whose `$1` is the folder itself.
///
/// Not `terminal-at`, its neighbour in the opener table: that one is for a
/// *file*, and opens in `$(dirname "$1")` — handed a folder, it would put the
/// terminal in the folder's parent.
pub const TERMINAL_OPENER: &str = "terminal-here";

/// An opener by its name in `[opener]`, detached from the config the way the
/// rules' choices are — the lookup for a verb that is about a folder rather
/// than about a file a rule matched.
pub fn named(config: &Config, name: &str) -> Option<Choice> {
    config.opener(name).map(Choice::from)
}

/// The built-in that only makes sense for several archives at once.
pub const MERGED_BUILTIN: &str = "extract-merged";

/// The choices worth offering when the targets hold `archives` archives:
/// "Extract all into one folder" of a single archive is "Extract to folder"
/// with a worse name, so it is dropped below two.
pub fn for_archives(mut choices: Vec<Choice>, archives: usize) -> Vec<Choice> {
    if archives < 2 {
        choices.retain(|choice| choice.builtin() != Some(MERGED_BUILTIN));
    }
    choices
}

// ── The `O` picker (`[pick]` context) ───────────────────────────────────────

/// The card that asks which opener to use, anchored to the row it is about.
#[derive(Debug, Clone)]
pub struct Picker {
    pub choices: Vec<Choice>,
    pub cursor: usize,
    /// The paths the chosen opener will be handed.
    pub paths: Vec<PathBuf>,
    /// The row the card points at — PLAN §6's "anchored to the hovered row".
    pub anchor: egui::Rect,
    /// The first choice drawn, when there are more than the window has room
    /// for.
    pub first: usize,
    /// How many choices the card showed when it was last drawn: a page, for
    /// the page keys ([`Picker::page`]).
    shown: Option<usize>,
    /// When the choices last scrolled, for their bar.
    bar: crate::scrollbar::Linger,
    /// The wheel's roll that has not come to a whole choice yet
    /// ([`crate::mouse::roll`]).
    carry: f32,
    /// The wheel has scrolled the choices off the cursor, and they stay where
    /// it left them until a key moves the cursor: the panes' rule
    /// ([`crate::tab::Listing::attach`]).
    detached: bool,
}

impl Picker {
    pub fn new(choices: Vec<Choice>, paths: Vec<PathBuf>, anchor: egui::Rect) -> Picker {
        Picker {
            choices,
            cursor: 0,
            paths,
            anchor,
            first: 0,
            shown: None,
            bar: crate::scrollbar::Linger::default(),
            carry: 0.0,
            detached: false,
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.choices.is_empty() {
            return;
        }
        let last = self.choices.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
        self.detached = false;
    }

    /// Put the cursor on choice `index`, a page key's or a click's: a cursor
    /// command, so the view comes back to it.
    pub fn set_cursor(&mut self, index: usize) {
        self.cursor = index.min(self.choices.len().saturating_sub(1));
        self.detached = false;
    }

    pub fn chosen(&self) -> Option<&Choice> {
        self.choices.get(self.cursor)
    }

    /// The card was laid out showing `shown` choices: the view follows the
    /// cursor in those, so it never walks below the last one drawn — unless
    /// the wheel has taken it off the cursor, when it is only kept inside the
    /// choices.
    pub fn fit(&mut self, shown: usize, now: std::time::Instant) {
        self.shown = Some(shown);
        self.first = if self.detached {
            self.first.min(self.choices.len().saturating_sub(shown))
        } else {
            crate::viewport::first_visible(self.first, self.cursor, self.choices.len(), shown, 0)
        };
        self.bar.saw(self.first as f32, now);
    }

    /// The wheel over the card, in points: whole choices at a time
    /// ([`crate::mouse::roll`]), the view leaving the cursor where it was.
    /// Returns whether the choices moved.
    pub fn wheel(&mut self, points: f32, now: std::time::Instant) -> bool {
        let rows = crate::mouse::wheel_rows(points, chrome::CARD_ROW);
        let last = self.choices.len().saturating_sub(self.page());
        let first = crate::mouse::roll(self.first, last, &mut self.carry, rows);
        self.scroll_to(first, now)
    }

    /// Start the choices at `first`, kept inside them, off the cursor until
    /// a key moves it. Returns whether they moved.
    pub fn scroll_to(&mut self, first: usize, now: std::time::Instant) -> bool {
        let first = first.min(self.choices.len().saturating_sub(self.page()));
        if first == self.first {
            return false;
        }
        self.first = first;
        self.detached = true;
        self.bar.saw(first as f32, now);
        true
    }

    /// A page of choices, for the page keys: the rows the card showed when
    /// it was last drawn — every choice in a window tall enough for them, as
    /// many as fit in one that is not.
    pub fn page(&self) -> usize {
        self.shown.unwrap_or(self.choices.len())
    }

    /// When the choices last scrolled, for their bar's linger.
    pub fn scrolled_at(&self) -> Option<std::time::Instant> {
        self.bar.scrolled_at()
    }

    /// A hand let go of the bar: it lingers from now
    /// ([`crate::scrollbar::Linger::let_go`]).
    pub fn let_go(&mut self, now: std::time::Instant) {
        self.bar.let_go(now);
    }
}

/// The picker's card and its rows, anchored under (or over) the row it is
/// about. Shared by paint and hit test.
///
/// As many rows as there are choices, and as many as the window has room for
/// ([`crate::dialog::fit_rows`]): a longer list scrolls with the cursor
/// rather than hanging the card off the window's edge.
pub fn picker_geometry(
    area: egui::Rect,
    anchor: egui::Rect,
    count: usize,
) -> (egui::Rect, Vec<egui::Rect>) {
    /// Wide enough for "Open in Zed" plus its opener name.
    const WIDTH: f32 = 260.0;
    let fixed = CARD_PAD * 2.0 + chrome::HINT_ROW;
    let (rows, fitted) =
        crate::dialog::fit_rows(area, WIDTH, fixed, chrome::CARD_ROW, count.max(1));
    let (width, height) = (fitted.width(), fitted.height());
    // Below the row, unless there is no room below — then above it, so the card
    // never covers the file it is offering to open.
    let below = anchor.bottom() + 4.0;
    let top = if below + height <= area.bottom() - chrome::CARD_MARGIN {
        below
    } else {
        (anchor.top() - 4.0 - height).max(area.top() + chrome::CARD_MARGIN)
    };
    let left = anchor
        .left()
        .min(area.right() - chrome::CARD_MARGIN - width)
        .max(area.left() + chrome::CARD_MARGIN);
    let card = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(width, height));
    let rects = (0..count.min(rows))
        .map(|i| {
            egui::Rect::from_min_size(
                egui::pos2(
                    card.left() + CARD_PAD,
                    card.top() + CARD_PAD + i as f32 * chrome::CARD_ROW,
                ),
                egui::vec2(card.width() - CARD_PAD * 2.0, chrome::CARD_ROW),
            )
        })
        .collect();
    (card, rects)
}

/// The picker's bar, beside its rows, while there are more choices than it
/// shows.
pub fn picker_bar(
    card: egui::Rect,
    rects: &[egui::Rect],
    picker: &Picker,
) -> Option<crate::scrollbar::Geometry> {
    crate::scrollbar::card(
        card,
        body(rects)?,
        picker.first as f32,
        rects.len() as f32,
        picker.choices.len() as f32,
    )
}

/// Where the picker's bar is pointed at by, while there are more of its
/// `count` choices than rows ([`crate::scrollbar::band`]).
pub fn picker_band(card: egui::Rect, rects: &[egui::Rect], count: usize) -> Option<egui::Rect> {
    crate::scrollbar::band(card, body(rects)?, rects.len() as f32, count as f32)
}

/// The rows, as one rect.
fn body(rects: &[egui::Rect]) -> Option<egui::Rect> {
    let (first, last) = (rects.first()?, rects.last()?);
    Some(egui::Rect::from_min_max(first.min, last.max))
}

/// Draw the picker.
pub fn paint_picker(
    paint: &Painting<'_>,
    card: egui::Rect,
    rects: &[egui::Rect],
    picker: &Picker,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let painter = paint.painter;
    let palette = paint.palette;
    chrome::card(paint, card, 1.0);
    if picker.choices.is_empty() {
        painter.text(
            card.center(),
            egui::Align2::CENTER_CENTER,
            "No opener rule matches",
            egui::FontId::proportional(FONT),
            palette.overlay0,
        );
        return;
    }
    for (i, rect) in rects.iter().enumerate() {
        let Some(choice) = picker.choices.get(picker.first + i) else {
            break;
        };
        // The row's place among the drawn ones, which is what the hit test
        // reports.
        let key = Control::PanelRow(i);
        let on_cursor = picker.first + i == picker.cursor;
        let hover = hovers.hover(key);
        let rect = pressed_rect(*rect, hovers.press(key));
        if on_cursor || hover > 0.0 {
            painter.rect_filled(
                rect,
                chrome::CARD_ROW_RADIUS,
                mix(
                    if on_cursor {
                        palette.surface1
                    } else {
                        palette.crust
                    },
                    palette.surface0,
                    hover,
                ),
            );
        }
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }
        // The description is what the picker is read for; the opener's name is
        // the thing you would write in `delightfile.toml`, so it goes quietly
        // on the right where it can be found but not tripped over.
        let name = inside.layout_no_wrap(
            choice.name.clone(),
            egui::FontId::monospace(FONT - 1.0),
            palette.overlay0,
        );
        inside.galley(
            egui::pos2(
                rect.right() - PAD_X - name.size().x,
                rect.center().y - name.size().y / 2.0,
            ),
            name.clone(),
            palette.overlay0,
        );
        chrome::truncated(
            &inside,
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            &choice.description,
            if on_cursor {
                palette.text
            } else {
                palette.subtext0
            },
            (rect.width() - PAD_X * 2.0 - name.size().x - 8.0).max(0.0),
        );
    }
    if let Some(bar) = picker_bar(card, rects, picker) {
        crate::scrollbar::paint_card(
            paint,
            &bar,
            crate::scrollbar::Surface::Picker,
            hovers,
            picker.scrolled_at(),
            1.0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract every opener rule in the shipped config depends on: the
    /// snippet is `-c`'s argument, `$0` is delightfile, and the paths start at
    /// `$1` — never spliced into the string.
    #[test]
    fn paths_reach_the_shell_as_arguments() {
        let paths = vec![
            PathBuf::from("/home/brian/a b.txt"),
            PathBuf::from("/home/brian/; rm -rf ~"),
        ];
        let argv = shell_argv("/bin/zsh", r#"zeditor "$@""#, &paths);
        assert_eq!(
            argv,
            vec![
                "/bin/zsh".to_string(),
                "-c".to_string(),
                r#"zeditor "$@""#.to_string(),
                "delightfile".to_string(),
                "/home/brian/a b.txt".to_string(),
                "/home/brian/; rm -rf ~".to_string(),
            ],
            "a semicolon in a file name is a character in an argument, not a command"
        );
        // `$1` is the first path, which is what the single-file openers use.
        assert_eq!(argv[4], "/home/brian/a b.txt");
        // …and with no selection the snippet still runs, with no positionals.
        assert_eq!(shell_argv("/bin/sh", "ls", &[]).len(), 4);
    }

    /// "Open terminal here" is the folder's own opener, whose `$1` is the
    /// folder — not the file's, which would open in the folder's parent — and
    /// a config without it has nothing to find rather than something else.
    #[test]
    fn the_terminal_opener_is_the_one_a_folder_row_offers() {
        let config = Config::default();
        let choice = named(&config, TERMINAL_OPENER).expect("shipped");
        assert_eq!(choice.name, "terminal-here");
        assert!(!choice.block, "a terminal must not hold the window");
        assert!(
            choice.command.contains(r#"--working-directory="$1""#),
            "{}",
            choice.command
        );
        assert!(!choice.command.contains("dirname"), "{}", choice.command);
        let folder_rule = config.openers_for("Work", "inode/directory", true);
        assert!(folder_rule.iter().any(|o| o.name == TERMINAL_OPENER));

        let mut bare = Config::default();
        bare.openers.retain(|o| o.name != TERMINAL_OPENER);
        assert_eq!(named(&bare, TERMINAL_OPENER), None);
    }

    #[test]
    fn detaching_only_prefixes_what_it_can_find() {
        let argv = shell_argv("/bin/sh", "true", &[]);
        let detached = detached_argv(argv.clone());
        if which("setsid").is_some() {
            assert_eq!(
                &detached[..2],
                &["setsid".to_string(), "--fork".to_string()]
            );
            assert_eq!(&detached[2..], &argv[..]);
        } else {
            assert_eq!(detached, argv, "no setsid: still open the file");
        }
    }

    /// PLAN §6's rules, as shipped: the first choice is what `o` runs and the
    /// whole list is what `O` offers.
    #[test]
    fn opener_rules_pick_by_glob_then_mime() {
        let config = Config::default();
        let names = |name: &str, mime: &str, is_dir: bool| -> Vec<String> {
            config
                .openers_for(name, mime, is_dir)
                .into_iter()
                .map(|o| o.name.clone())
                .collect()
        };
        // A 3D model is `text/plain` to every sniffer on the machine, which is
        // exactly why the by-name rule comes first.
        assert_eq!(
            names("teapot.obj", "text/plain", false),
            vec!["delightviewer", "open", "terminal-at"]
        );
        // Text opens in Zed; the terminal editor and the system default are
        // what `O` offers after it, and a shell in the file's folder is last.
        assert_eq!(
            names("notes.txt", "text/plain", false),
            vec!["zed", "edit", "open", "terminal-at"]
        );
        assert_eq!(
            names("index.html", "text/html", false),
            vec!["zed", "open-in-chrome", "edit", "open", "terminal-at"]
        );
        assert_eq!(
            names("Cargo.toml", "application/toml", false),
            vec!["zed", "edit", "open", "terminal-at"]
        );
        assert_eq!(names("cat.png", "image/png", false)[0], "delightviewer");
        assert_eq!(
            names("mystery", "application/octet-stream", false),
            vec!["open", "terminal-at"]
        );
        let dir = names("Work", "inode/directory", true);
        assert!(dir.len() > 1, "a directory has a picker's worth of choices");
        assert_eq!(dir.last().map(String::as_str), Some("terminal-here"));

        // **`edit` does not hold the window.** A blocking `$EDITOR` from a
        // program with no tty was a terminal editor with nowhere to draw; it
        // runs in a terminal of its own now, detached like every other launch.
        let edit = config.opener("edit").expect("edit");
        assert!(!edit.block, "edit must not block: {}", edit.command);
        assert!(edit.command.starts_with("setsid uwsm-app -- "));
        // …and nothing reveals: from a file manager, that opened another one.
        assert!(config.opener("reveal").is_none());
        assert!(
            config
                .rules
                .iter()
                .all(|rule| rule.openers.iter().all(|name| name != "reveal")),
            "a rule still names reveal"
        );
        // An archive gets the extract rule PLAN §6 says yazi was missing.
        let archive = names("backup.tar.gz", "application/gzip", false);
        assert_eq!(archive[0], "extract");
        assert_eq!(
            config.opener("extract").and_then(Opener::builtin),
            Some("extract"),
            "…and it is a built-in, not a shell command"
        );
    }

    fn file(name: &str) -> Entry {
        use df_core::fs::Kind;
        let mime = df_core::fs::mime::hint_for_name(name);
        Entry {
            name: name.to_string(),
            path: PathBuf::from("/dl").join(name),
            kind: Kind::File,
            len: 10,
            mtime: None,
            btime: None,
            mode: 0o644,
            uid: 0,
            gid: 0,
            is_hidden: false,
            mime,
            file_kind: df_core::fs::classify(Kind::File, name, mime, 0o644),
            tags: Vec::new(),
        }
    }

    /// Every archive gets the three extract built-ins, a middle piece of a set
    /// included, and the merged one only when there is something to merge.
    #[test]
    fn archives_offer_the_extract_builtins_and_merging_needs_two() {
        let config = Config::default();
        let ids = |choices: &[Choice]| -> Vec<String> {
            choices.iter().map(|c| c.name.clone()).collect()
        };
        let all = vec!["extract", "extract-here", "extract-merged", "open"];
        assert_eq!(ids(&choices_for(&config, &file("photos.zip"))), all);
        assert_eq!(ids(&choices_for(&config, &file("a.7z"))), all);
        // No extension a rule names and no mime: matched as `backup.7z`.
        assert_eq!(ids(&choices_for(&config, &file("backup.7z.002"))), all);
        assert_eq!(ids(&choices_for(&config, &file("photos.z01"))), all);
        assert_eq!(ids(&choices_for(&config, &file("bundle.tar.gz.003"))), all);

        let choices = choices_for(&config, &file("photos-1.zip"));
        assert_eq!(
            ids(&for_archives(choices.clone(), 1)),
            vec!["extract", "extract-here", "open"]
        );
        assert_eq!(ids(&for_archives(choices, 2)), all);
        assert_eq!(
            choices_for(&config, &file("photos.zip"))[0].builtin(),
            Some("extract")
        );
    }

    #[test]
    fn a_blocking_command_reports_its_exit_status() {
        assert_eq!(exit_text("ls -la", 0), "ls -la — done");
        assert_eq!(exit_text("false", 1), "false — exit 1");
        let long = "a".repeat(80);
        let text = exit_text(&long, 0);
        assert!(text.contains('…'), "long snippets are cut: {text}");
        assert_eq!(short("git   log \n --oneline"), "git log --oneline");
    }

    /// The card points at its row and stays on screen, above the row when there
    /// is no room below it.
    #[test]
    fn the_picker_anchors_to_its_row() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let row = egui::Rect::from_min_size(egui::pos2(300.0, 200.0), egui::vec2(400.0, 22.0));
        let (card, rects) = picker_geometry(area, row, 4);
        assert_eq!(rects.len(), 4);
        assert!(card.top() >= row.bottom(), "below the row it is about");
        assert!(area.contains_rect(card));

        let low = egui::Rect::from_min_size(egui::pos2(300.0, 880.0), egui::vec2(400.0, 22.0));
        let (card, _) = picker_geometry(area, low, 4);
        assert!(card.bottom() <= low.top(), "…or above it when it must be");

        // A row at the right-hand edge does not push the card off screen.
        let right = egui::Rect::from_min_size(egui::pos2(1380.0, 200.0), egui::vec2(20.0, 22.0));
        let (card, _) = picker_geometry(area, right, 2);
        assert!(card.right() <= area.right());
    }

    /// A short window gets the choices it has room for, with the card inside
    /// the window, and the cursor scrolls in those; a tall one shows them
    /// all. The bar is there only while choices are left out.
    #[test]
    fn a_short_window_scrolls_the_pickers_choices() {
        let choices: Vec<Choice> = (0..20)
            .map(|n| Choice {
                name: format!("opener-{n}"),
                command: "true".to_string(),
                description: format!("Open with number {n}"),
                block: false,
            })
            .collect();
        let row = egui::Rect::from_min_size(egui::pos2(300.0, 120.0), egui::vec2(400.0, 22.0));
        let mut picker = Picker::new(choices, Vec::new(), row);
        let window = |height: f32| {
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, height))
        };

        let tall = window(900.0);
        let (card, rects) = picker_geometry(tall, row, picker.choices.len());
        assert_eq!(rects.len(), picker.choices.len(), "all twenty fit");
        assert_eq!(picker_bar(card, &rects, &picker), None);
        assert_eq!(picker_band(card, &rects, picker.choices.len()), None);

        let short = window(300.0);
        let (card, rects) = picker_geometry(short, row, picker.choices.len());
        assert!(short.contains_rect(card), "{card:?}");
        assert!(!rects.is_empty() && rects.len() < picker.choices.len());
        assert!(rects.iter().all(|rect| card.contains_rect(*rect)));
        assert!(picker_bar(card, &rects, &picker).is_some());
        assert!(picker_band(card, &rects, picker.choices.len()).is_some());
        assert_eq!(
            picker.page(),
            picker.choices.len(),
            "undrawn, a page is all"
        );
        for _ in 0..picker.choices.len() {
            picker.move_cursor(1);
            picker.fit(rects.len(), std::time::Instant::now());
            assert!((picker.first..picker.first + rects.len()).contains(&picker.cursor));
        }
        // …and a page is the choices it showed, for the page keys.
        assert_eq!(picker.page(), rects.len());
    }

    #[test]
    fn the_picker_paints_without_panicking() {
        let choices: Vec<Choice> = Config::default()
            .openers_for("cat.png", "image/png", false)
            .into_iter()
            .map(Choice::from)
            .collect();
        let anchor = egui::Rect::from_min_size(egui::pos2(300.0, 200.0), egui::vec2(400.0, 22.0));
        let mut picker = Picker::new(choices, vec![PathBuf::from("/tmp/cat.png")], anchor);
        picker.move_cursor(1);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let painting = Painting {
                tips: None,
                held: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                tags: &crate::tags::BUILT_IN,
                nerd: false,
                show_symlink: true,
                now: std::time::Instant::now(),
            };
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
            let (card, rects) = picker_geometry(area, anchor, picker.choices.len());
            paint_picker(
                &painting,
                card,
                &rects,
                &picker,
                &Hovers::new(),
                &Ripples::new(),
            );
            let empty = Picker::new(Vec::new(), Vec::new(), anchor);
            let (card, rects) = picker_geometry(area, anchor, 0);
            paint_picker(
                &painting,
                card,
                &rects,
                &empty,
                &Hovers::new(),
                &Ripples::new(),
            );
        });
    }
}
