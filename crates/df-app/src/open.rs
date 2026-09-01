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
pub fn choices_for(config: &Config, entry: &Entry) -> Vec<Choice> {
    config
        .openers_for(&entry.name, entry.mime, entry.is_dir())
        .into_iter()
        .map(Choice::from)
        .collect()
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
}

impl Picker {
    pub fn new(choices: Vec<Choice>, paths: Vec<PathBuf>, anchor: egui::Rect) -> Picker {
        Picker {
            choices,
            cursor: 0,
            paths,
            anchor,
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.choices.is_empty() {
            return;
        }
        let last = self.choices.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    pub fn chosen(&self) -> Option<&Choice> {
        self.choices.get(self.cursor)
    }
}

/// The picker's card and its rows, anchored under (or over) the row it is
/// about. Shared by paint and hit test.
pub fn picker_geometry(
    area: egui::Rect,
    anchor: egui::Rect,
    count: usize,
) -> (egui::Rect, Vec<egui::Rect>) {
    /// Wide enough for "Open in Zed" plus its opener name.
    const WIDTH: f32 = 260.0;
    let rows = count.max(1);
    let height = CARD_PAD * 2.0 + rows as f32 * chrome::CARD_ROW;
    let width = WIDTH.min(area.width() - chrome::CARD_MARGIN * 2.0);
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
    let card = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(width.max(0.0), height));
    let rects = (0..count)
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
        let Some(choice) = picker.choices.get(i) else {
            break;
        };
        let key = Control::PanelRow(i);
        let on_cursor = i == picker.cursor;
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
            vec!["delightviewer", "reveal"]
        );
        assert_eq!(names("cat.png", "image/png", false)[0], "delightviewer");
        let dir = names("Work", "inode/directory", true);
        assert!(dir.len() > 1, "a directory has a picker's worth of choices");
        // An archive gets the extract rule PLAN §6 says yazi was missing.
        let archive = names("backup.tar.gz", "application/gzip", false);
        assert_eq!(archive[0], "extract");
        assert_eq!(
            config.opener("extract").and_then(Opener::builtin),
            Some("extract"),
            "…and it is a built-in, not a shell command"
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
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
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
