//! More than one window (PLAN §2): `Ctrl+N`, dragging a tab out of the strip,
//! and what a second window costs.
//!
//! # The decision: a window is a **process**
//!
//! winit can drive several windows off one event loop — `ApplicationHandler`
//! routes by [`WindowId`](winit::window::WindowId) — so "a second window" could
//! have been a second [`Gfx`](crate::graphics::Gfx) and a second set of tabs
//! inside this process. It is not. Each window is a whole new `delightfile`
//! started with [`std::process::Command`], on the directory the gesture asked
//! for. What follows is the evidence, because this is the kind of decision that
//! looks like laziness until you count.
//!
//! **What in-process multi-window would have cost.** The unit of state in this
//! program is [`App`](crate::app::App): one struct, ~80 fields, 232 methods,
//! every one of them `&mut self`, and it mixes the two halves the refactor
//! would have to separate:
//!
//! - *Per process*: the scanner, the watcher, the task engine, the undo
//!   journal, the clipboard, the basket, the config, the keymap registry, the
//!   state store, the git front end, the vfs, the du scanner, the udisks
//!   worker.
//! - *Per window*: `gfx`, `tabs`, `focus`, the preview pane and its decoders,
//!   the player, every overlay (`help`, `finder`, `search`, `dialog`,
//!   `picker`, `panel`, `spot`, `menu`, `mounts`), the hover, ripple and
//!   target tracks, `zones`, `drag`, `flip`, `path_bar`, `cursor_rect`.
//!
//! Splitting those means rewriting the signature of essentially every method in
//! `app.rs` — `&mut self` becomes `(&mut Shared, &mut Window)` — which is a
//! rewrite of the largest and most load-bearing file in the tree, in the phase
//! that is supposed to be *finishing* it. Three more things would have had to
//! change with it:
//!
//! 1. [`crate::wayland::DataDevice`] is built around **one** origin
//!    `wl_surface`: it adopts a single surface pointer for `start_drag`, and it
//!    tracks a single incoming offer for that surface. Two windows means a
//!    surface→window map, per-surface enter/leave routing and per-window
//!    serials — protocol work in the one file where the compiler is not
//!    checking (`#![allow(unsafe_code)]`).
//! 2. [`Gfx::new`](crate::graphics::Gfx::new) creates its own wgpu instance,
//!    adapter and device. A second window that did not share them would be a
//!    second GPU device per window; sharing them is another refactor.
//! 3. The preview pane owns a [`Player`](crate::playback::Player) with a cpal
//!    output stream. Two windows means two of those, and an audio-focus rule
//!    that does not exist yet.
//!
//! **What the process model gets for free.** Cross-window drag-and-drop already
//! works, because Phase 4b's drag-out goes through the compositor: a drag out
//! of one window offers `text/uri-list` on a `wl_data_source`, and a drop into
//! another takes `text/uri-list` off a `wl_data_offer` ([`crate::dnd`]). The
//! compositor does not care whether the two ends are the same process, so
//! window→window file drags are the *external* path that Phase 4b already
//! ships — the only thing that had to change is that "is this drag mine?" now
//! means "is it *this process's*" rather than "is it some delightfile's" (see
//! [`crate::dnd::self_mime`]).
//!
//! **What it costs, honestly.** A second window is a second process, so it has
//! its own undo journal, its own basket, its own cut/copy clipboard and its own
//! task engine: a copy started in window A does not appear in window B's `w`
//! panel, and `u` in B cannot undo what A did. It is also a second wgpu device
//! and a second set of workers — tens of megabytes, not hundreds, and nothing
//! at rest, since every worker in this program is lazy or event-driven. Those
//! are real limitations and they are the price of not destabilising `app.rs`;
//! if shared undo across windows is ever wanted, that is the moment to pay for
//! the split, with the shape above as the map.
//!
//! # The state file, with several processes
//!
//! Every process loads [`StateStore`](df_core::state::StateStore) at startup and
//! rewrites the whole file behind the app's debounce. So it is **last writer
//! wins**: two windows that both toggle a grid in the same second agree on
//! whichever wrote second, and the `!tabs` record is the last-quitting window's
//! tab list. That is the right trade for what this file *is* — a preferences
//! file whose loss costs a shrug (`df_core::state`'s own words) — and the write
//! is atomic (temp file, then `rename`), so the failure mode is a forgotten
//! grid toggle, never a corrupt file. Reconciling per-record would mean a
//! re-read-and-merge on every flush and a mtime check, which is a lock protocol
//! for the benefit of a memo about which folders look nice as thumbnails.
//!
//! # The cwd-file, with several processes
//!
//! **The process launched with `--cwd-file` owns it, and windows it spawns
//! never inherit it** ([`spawn_args`] does not pass it on). The flag exists for
//! one thing — Brian's Hyprland `Super+F` runs delightfile with it and `cd`s the
//! shell to whatever came back (PLAN §3) — and that shell function waits on the
//! *one* pid it started. If every window wrote the file, the shell would follow
//! whichever window happened to be closed last, which is not a thing the person
//! at the keyboard can predict. This way `q` in the window you launched writes
//! the directory that window ended in, exactly as with one window, and windows
//! dragged out of it are ordinary windows that quit without saying anything.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Child, Command};

// ── Spawning ────────────────────────────────────────────────────────────────

/// The command line a new window is started with.
///
/// Two arguments, and the first one is the reason this is a function rather
/// than a `.arg(dir)` at the call site: `--` ends the options, so a directory
/// genuinely called `--cwd-file=/tmp/x` is opened rather than parsed. The
/// child gets **no** `--cwd-file` of its own — see this module's header.
pub fn spawn_args(dir: &Path) -> Vec<OsString> {
    vec![OsString::from("--"), dir.as_os_str().to_os_string()]
}

/// Every window this process has opened, so none of them becomes a zombie.
///
/// A `Child` that is never waited on stays in the process table after it exits.
/// Nothing here waits *for* a window — that would freeze the window that opened
/// it — so the handles are kept and reaped with `try_wait` the next time one is
/// opened, which is the only moment the list can have grown.
#[derive(Default)]
pub struct Windows {
    children: Vec<Child>,
}

impl Windows {
    /// Open a window on `dir`. Errors are the caller's to report — it is the
    /// one that knows whether a toast is warranted.
    pub fn open(&mut self, dir: &Path) -> std::io::Result<()> {
        self.reap();
        let exe = std::env::current_exe()?;
        let mut command = Command::new(exe);
        command.args(spawn_args(dir));
        // The child starts *in* the directory it is opening, so a shell command
        // run from it (`;`, `:`) inherits the cwd a person would expect, and so
        // a relative path in a later argument would mean the same thing there
        // as here. Not set when the directory is gone: a `current_dir` that
        // does not exist fails the spawn outright, and an unopenable directory
        // should still get a window that says so.
        if dir.is_dir() {
            command.current_dir(dir);
        }
        let child = command.spawn()?;
        self.children.push(child);
        Ok(())
    }

    /// Collect the windows that have already closed.
    fn reap(&mut self) {
        self.children.retain_mut(|child| {
            // `Ok(Some(_))` is "it has exited and has now been waited on";
            // `Err` means it cannot be waited on at all, and either way there
            // is nothing left to hold.
            !matches!(child.try_wait(), Ok(Some(_)) | Err(_))
        });
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.children.len()
    }
}

// ── Dragging a tab out (PLAN §2) ────────────────────────────────────────────

/// How far a tab chip must travel *vertically* before letting go of it opens a
/// window.
///
/// 40 pt is a little under two tab-strip heights, and it is deliberately four
/// times [`crate::mouse::DRAG_THRESHOLD`]: the small threshold is what turns a
/// press into a gesture, and this one is what turns a gesture into an action
/// with a consequence — opening a window and closing a tab is not something a
/// twitch may do. Measured from where the button went down, so a slow drag and
/// a flick arm at the same place.
pub const DETACH_THRESHOLD: f32 = 40.0;

/// Whether a tab drag from `from`, now at `at`, would detach if it were let go.
///
/// Three conditions, and the second two are the defence:
///
/// - It has travelled [`DETACH_THRESHOLD`] vertically. Up counts as well as
///   down: the strip is at the top of the window, and a hand that flicks a chip
///   upward off it has said the same thing as one that pulls it down.
/// - The vertical travel is greater than the horizontal. A drag *along* the
///   strip is a hand sliding between chips, which must never open a window —
///   and it leaves the horizontal axis free for tab reordering later.
/// - The pointer is outside the strip. The strip is where tabs live; a gesture
///   that ends inside it has not taken the tab anywhere.
pub fn armed(from: egui::Pos2, at: egui::Pos2, strip: egui::Rect) -> bool {
    let travel = at - from;
    travel.y.abs() >= DETACH_THRESHOLD && travel.y.abs() > travel.x.abs() && !strip.contains(at)
}

/// What letting go of a tab drag does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// Open a window on the tab and close it here.
    Detach,
    /// Nothing happened; the ghost flies home (`delightful-ui` §6).
    SpringBack,
}

/// The release, from the same three facts [`armed`] is decided on — so what the
/// ghost has been advertising all through the drag is what letting go does.
pub fn release(from: egui::Pos2, at: egui::Pos2, strip: egui::Rect) -> Release {
    if armed(from, at, strip) {
        Release::Detach
    } else {
        Release::SpringBack
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli;
    use std::path::PathBuf;

    /// The child's command line must survive this program's own parser — a
    /// directory whose name looks like a flag included.
    #[test]
    fn a_spawned_window_opens_the_directory_it_was_given() {
        let round_trip = |dir: &str| {
            let args = spawn_args(Path::new(dir));
            let strings: Vec<String> = args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            cli::parse(strings)
        };
        assert_eq!(
            round_trip("/home/brian/src"),
            cli::Outcome::Run(cli::Args {
                start: Some(PathBuf::from("/home/brian/src")),
                cwd_file: None,
            })
        );
        // The `--` is why this one is a directory and not a parse error.
        assert_eq!(
            round_trip("--cwd-file=/tmp/x"),
            cli::Outcome::Run(cli::Args {
                start: Some(PathBuf::from("--cwd-file=/tmp/x")),
                cwd_file: None,
            })
        );
    }

    /// The cwd-file policy, in the one line that states it: a window opened
    /// from another window never writes the shell's directory.
    #[test]
    fn a_spawned_window_never_inherits_the_cwd_file() {
        let args = spawn_args(Path::new("/tmp"));
        assert!(
            !args.iter().any(|a| a.to_string_lossy().contains("cwd-file")),
            "{args:?}"
        );
    }

    fn strip() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(600.0, 26.0))
    }

    #[test]
    fn a_tab_detaches_only_when_it_has_really_been_pulled_out() {
        let strip = strip();
        let from = egui::pos2(100.0, 20.0);
        // Straight down, past the threshold, out of the strip.
        let out = egui::pos2(100.0, 20.0 + DETACH_THRESHOLD);
        assert!(armed(from, out, strip));
        assert_eq!(release(from, out, strip), Release::Detach);
        // Upward counts too.
        assert!(armed(from, egui::pos2(100.0, 20.0 - DETACH_THRESHOLD), strip));

        // A point short of the threshold does not arm…
        let short = egui::pos2(100.0, 20.0 + DETACH_THRESHOLD - 0.5);
        assert!(!armed(from, short, strip));
        assert_eq!(release(from, short, strip), Release::SpringBack);
        // …nor does a hand sliding along the strip, however far it slides.
        let along = egui::pos2(500.0, 22.0);
        assert!(!armed(from, along, strip));
        // …nor a mostly-horizontal drag that happens to clear the threshold.
        let diagonal = egui::pos2(100.0 + DETACH_THRESHOLD * 2.0, 20.0 + DETACH_THRESHOLD + 1.0);
        assert!(!armed(from, diagonal, strip));
        assert_eq!(release(from, diagonal, strip), Release::SpringBack);
        // …nor one that ends back inside the strip after a trip below it.
        let returned = egui::pos2(300.0, 20.0);
        assert!(!armed(from, returned, strip));
        assert_eq!(release(from, returned, strip), Release::SpringBack);
    }

    /// The threshold is the *defended* one, not the one that starts the drag.
    #[test]
    fn the_detach_threshold_is_larger_than_the_drag_threshold() {
        const { assert!(DETACH_THRESHOLD > crate::mouse::DRAG_THRESHOLD * 2.0) };
    }

    /// Nothing has been opened, so nothing is held — the registry starts empty
    /// and stays empty until a window is asked for.
    #[test]
    fn the_registry_holds_nothing_until_a_window_is_opened() {
        let windows = Windows::default();
        assert_eq!(windows.len(), 0);
    }
}
