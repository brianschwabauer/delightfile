//! The event loop: winit's [`ApplicationHandler`] and the one place a frame is
//! decided on.
//!
//! The rule this file exists to enforce (PLAN §1) is **repaint on event, never
//! poll**. `ControlFlow::Wait` is the resting state; a frame happens because
//! something happened — a key, a pointer move, a resize, a worker ringing the
//! [`Wake`](crate::Wake) bell — or because an animation asked for its next one
//! by a deadline (`ControlFlow::WaitUntil`). An idle delightfile costs zero
//! repaints, and every animation added later has to be able to say when it has
//! arrived, or it breaks that.
//!
//! It is also the router: keystrokes arrive as winit events, become df-core
//! [`Chord`]s, and go through the keymap registry to a [`Command`] that this
//! file executes against the model. Phase 1 implements the browsing subset;
//! everything else is logged and ignored, which is deliberate — a command that
//! silently did *nearly* the right thing would be worse than one that has not
//! landed yet.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use df_core::config::{Config, LineMode, MgrConfig, SortBy, Theme};
use df_core::fs::{random_seed, FindDirection, Scanner, SortOptions, WatchEvent, Watcher};
use df_core::input::{InputBuffer, InputEvent};
use df_core::keymap::{
    Chord, Command, Context, ContextStack, Dispatch, Key, KeymapState, Registry, WhenFlags,
};
use df_core::ops::journal::{Fingerprint, Journal, MovedPath, OpRecord};
use df_core::ops::paste::{plan_paste, Clipboard, PasteMode};
use df_core::ops::{DeleteJob, LinkKind, Outcome, PasteJob, TrashJob};
use df_core::preview::PreviewKind;
use df_core::state::{StateStore, View};
use df_core::tasks::{FnJob, Lane, TaskCtx, TaskEngine, TaskEvent, TaskId, TaskState};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::chrome;
use crate::dialog::{self, Confirm, ConfirmKind, ConflictDialog, Step};
use crate::dnd;
use crate::finder::{self, Choice, Finder, Source};
use crate::flip::{self, Flip, Snapshot};
use crate::focus::{escape_rung, EscapeRung, EscapeState, Focus, Hovered, Rightward};
use crate::graphics::{Gfx, GfxError};
use crate::grid::{self, GridView, Thumbs};
use crate::help::{self, Help};
use crate::hover::Hovers;
use crate::input::{Prompt, PromptKind};
use crate::menu::{self, Menu};
use crate::open::{self, Picker};
use crate::overlay::{self, FinderGeom, SearchGeom};
use crate::panel::{self, TaskPanel, TaskRow};
use crate::playback::{Player, Prober, TemporalInfo};
use crate::preview::Pane as PreviewPane;
use crate::ripple::Ripples;
use crate::search::{self, Search};
use crate::select::{self, Visual};
use crate::spot::{self, Spot};
use crate::tab::Tab;
use crate::tabs::Tabs;
use crate::theme::Palette;
use crate::toast::Toasts;
use crate::ui::{self, ClipMark, Column, Control, CursorGlow, ListView};
use crate::whichkey::WhichKey;

/// Opening size, in logical pixels. Wide enough for the `[1, 4, 3]` miller
/// columns (PLAN §2) to each be usable at once — the middle column is the one
/// being read, and at much under this the preview stops being worth its share.
/// Tiled compositors override it immediately; it only decides the floating and
/// first-run case.
const WINDOW_SIZE: (f64, f64) = (1400.0, 900.0);

/// Wayland `app_id`. Matches the desktop file and whatever window rule the user
/// writes, so it must never change casually.
const APP_ID: &str = "delightfile";

/// Ignore an egui repaint deadline further out than this and just go to sleep.
/// egui signals "no repaint needed" as a duration near `Duration::MAX`; any
/// real animation is milliseconds away, so anything past an hour is that
/// sentinel wearing a number.
const REPAINT_HORIZON: Duration = Duration::from_secs(3600);

/// The first wait after a frame could not be presented; each consecutive
/// failure adds another, capped at eight (see `redraw`).
const PRESENT_RETRY: Duration = Duration::from_millis(250);

/// How long a directory read may take before the pane admits it is reading.
///
/// The first batch is 64 entries (`df_core::fs::FIRST_BATCH`) and normally
/// lands inside a millisecond, so a "loading" label would be a flash nobody can
/// read and everybody notices. 150 ms is the usual threshold for "this is
/// taking a moment" — long enough that a local directory never trips it, short
/// enough that a sleeping disk or a dead NFS mount does not leave the pane
/// looking empty and wrong.
const LOADING_DELAY: Duration = Duration::from_millis(150);

/// How many probed files are remembered (see [`App::probes`]).
///
/// Four: a `↓ ↑`, and a step back up out of a directory onto the clip you were
/// just on. Past that, asking ffmpeg again is cheaper than the memory of it.
const PROBE_MEMORY: usize = 4;

/// How strongly the band-select rectangle tints what it is over.
///
/// Six per cent: enough that the rectangle reads as a *region* rather than as
/// four lines, faint enough that the file names under it are still readable —
/// which matters, because the whole point of dragging a band is watching which
/// rows it takes.
const BAND_FILL: f32 = 0.06;

/// …and its edge. Much stronger than the fill, because the edge is the part
/// that says where the rectangle *ends*, which is the thing being aimed.
const BAND_EDGE: f32 = 0.55;

/// Wakes the event loop from a worker thread.
///
/// The **only** cross-thread wakeup mechanism in delightfile (PLAN §1). Workers
/// publish their results on their own channels and then ring this bell; the
/// loop drains the channels in `user_event`. The alternative — a short
/// `WaitUntil` poll while any work is in flight — is what delightviewer removed
/// when playback arrived, and starting without it means never having to.
///
/// Cheap to clone, and safe to hold after the loop has exited: a send to a
/// dead proxy is an error this deliberately drops, because "the window is
/// gone" is not something a worker can or should do anything about.
#[derive(Clone)]
pub struct Waker {
    ring: Arc<dyn Fn() + Send + Sync>,
    /// Which worker this handle belongs to, for `DF_FRAME_LOG` only.
    ///
    /// The bell is deliberately anonymous to the event loop — `Wake` carries no
    /// payload, and `user_event` is one branch wide no matter how many workers
    /// exist. But "which worker rang, and did it have a frame's worth of work?"
    /// is precisely the question PLAN §6's idle-cost audit has to answer, and
    /// an unlabelled bell makes a spuriously-ringing worker invisible: the log
    /// says a wake happened and nothing about who caused it. So the *handle*
    /// carries a name the *event* does not.
    source: &'static str,
}

impl Waker {
    pub fn new(proxy: EventLoopProxy<crate::Wake>) -> Waker {
        Waker {
            ring: Arc::new(move || {
                let _ = proxy.send_event(crate::Wake);
            }),
            source: "root",
        }
    }

    /// The same bell, labelled with the worker about to be handed it.
    ///
    /// Clone-and-rename rather than a parameter on [`wake`](Waker::wake): a
    /// worker is given its handle once, at startup, and then rings it from a
    /// thread that has no idea what it is called.
    pub fn named(&self, source: &'static str) -> Waker {
        Waker {
            ring: Arc::clone(&self.ring),
            source,
        }
    }

    /// Ask the event loop for a pass through `user_event`.
    pub fn wake(&self) {
        if frame_log_enabled() {
            log::info!("wake: {}", self.source);
        }
        (self.ring)()
    }
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waker")
    }
}

/// One keystroke, as both of the things a keystroke can be.
///
/// A key that is bound is a [`Chord`]; a key that is *typed into something* is
/// text. They are not alternatives — `f` is a command in the browser and the
/// letter `f` in a filter box, and which one it is depends on state the key
/// handler does not have. So both readings are carried through to the router,
/// which is the one place that knows whether anything is open to type into.
///
/// `text` is `None` for anything that is not printable — control characters
/// arrive on winit's `text` field too, and `Esc` inserting `\u{1b}` into a
/// filter is the kind of bug that is invisible until somebody's query stops
/// matching for no reason.
#[derive(Debug, Clone)]
struct Press {
    /// winit's own answer to "is this key repeating", which is the only
    /// honest source for it: a timer here would guess.
    repeat: bool,
    chord: Option<Chord>,
    text: Option<String>,
}

/// A job that has been spawned and whose result the UI still owes somebody a
/// toast for.
///
/// The engine's event stream says a task is *over*; the outcome slot says what
/// it did. Both are needed: the record for the journal and the message for the
/// toast are in the slot, and the moment to read them is the event.
struct PendingOp {
    id: TaskId,
    slot: Outcome,
    /// Directories to re-read once it lands. inotify usually beats us to it,
    /// which is why `rescan` is idempotent — a second read of a directory that
    /// is already right costs one scan and changes nothing.
    dirs: Vec<PathBuf>,
}

/// An `archive::list` running on the pool, and what its result is for.
///
/// The listing is a job like any other because it is a *read of a whole file*:
/// a 400 MB `.tar.zst` is streamed through `zstd` and parsed block by block, and
/// doing that on the event loop would be `→` freezing the window for a second.
struct PendingArchive {
    id: TaskId,
    path: PathBuf,
    intent: ArchiveIntent,
    slot: Arc<std::sync::Mutex<Option<std::result::Result<df_core::archive::ArchiveTree, String>>>>,
}

/// Why an archive is being listed.
///
/// Extraction needs the tree as much as browsing does — [`df_core::archive::plan_extract`] is a
/// function of it — so "extract this archive without opening it" is the same
/// read with a different ending, and it is spelled as one here rather than as a
/// second pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveIntent {
    /// Walk into it.
    Browse,
    /// Unpack it into the directory it lives in.
    ExtractHere,
    /// Unpack it into a new folder named after it.
    ExtractSubfolder,
}

/// A remote operation running on the pool (PLAN §7.6), and its result slot.
///
/// The vfs's own calls are blocking and take a [`TaskCtx`], which its
/// documentation says is because they are meant to run inside a job. This is
/// that job's bookkeeping: an `ssh` round trip must never happen between two
/// frames, so *every* remote verb — including the one-packet ones like `MKDIR`
/// — goes onto the pool and comes back through here.
struct PendingRemote {
    id: TaskId,
    slot: Arc<std::sync::Mutex<Option<std::result::Result<RemoteDone, String>>>>,
}

/// What a finished remote operation owes the UI.
///
/// One flat shape rather than a variant per verb, because every remote
/// operation answers the same four questions and a rename that forgot to
/// invalidate its directory is the bug this makes structurally hard.
#[derive(Debug, Default)]
struct RemoteDone {
    /// One line for the toast. Empty says nothing.
    message: String,
    /// The remote directory whose cached listing is now wrong.
    invalidate: Option<df_core::vfs::VfsPath>,
    /// Local directories to re-read — a download's destination.
    dirs: Vec<PathBuf>,
    /// A file downloaded in order to be opened: `(remote url, local path)`.
    open: Option<(String, PathBuf)>,
    /// A file downloaded for the preview card: `(remote url, local path)`.
    preview: Option<(String, PathBuf)>,
    /// An upload that has looked at its destination and now needs the plan
    /// built — and, if anything is in the way, the conflict dialog put up.
    upload: Option<UploadProbe>,
}

/// What the server said about the names an upload is about to write.
///
/// The round trip has to happen *before* the transfer and cannot happen on the
/// UI thread, so the question and the answer are two frames apart: this is the
/// answer, carried back through the same slot every other remote verb uses.
#[derive(Debug)]
struct UploadProbe {
    /// The directory the files are going into.
    dest: df_core::vfs::VfsPath,
    /// The local files, in the order they were yanked.
    sources: Vec<PathBuf>,
    /// The rows already in `dest` whose names an upload would take — only
    /// those, because a stat per source is the whole cost of asking.
    taken: Vec<df_core::fs::Entry>,
}

/// The preview card's state for the remote row under the cursor.
struct RemotePreview {
    /// The row it is about, as its `sftp://…` URL.
    url: String,
    /// The text, once it has arrived.
    body: Option<String>,
    /// Whether a download for it is in flight, so the card can say "reading…"
    /// instead of showing the facts as though they were the answer.
    loading: bool,
}

/// The modal card that is up, if one is.
enum Dialog {
    /// `d` / `D`.
    Confirm(Confirm),
    /// A paste that hit a name that is taken. Boxed: it carries a whole
    /// [`PastePlan`](df_core::ops::paste::PastePlan) and the enum is otherwise
    /// a few words wide.
    Conflict(Box<ConflictDialog>),
    /// `r` on a multi-selection: the two-column rename diff (PLAN §5). Boxed
    /// for the same reason — it carries a line editor per row.
    Bulk(Box<crate::bulk::Bulk>),
}

/// One frame's worth of "where the open surface's pieces are".
enum OverlayGeom {
    Confirm(dialog::Geometry),
    Conflict(dialog::Geometry),
    Bulk(dialog::Geometry),
    Picker(egui::Rect, Vec<egui::Rect>),
    Panel(egui::Rect, Vec<egui::Rect>, Vec<TaskRow>),
    Spot(spot::Geometry),
    /// The fuzzy card (PLAN §4.4).
    Finder(FinderGeom),
    /// The disks card (PLAN §7.4).
    Mounts(crate::mounts::Geometry),
    /// The `s` / `S` panel (PLAN §7.2). Boxed for the same reason the conflict
    /// dialog is: it carries a row rectangle per visible hit and the enum is
    /// otherwise a few words wide.
    Search(Box<SearchGeom>),
}

impl OverlayGeom {
    /// What the pointer is over. `None` inside the card but not on anything is
    /// still "inside the card" as far as the caller is concerned — the modal
    /// swallows the pointer either way (see the hit test in `frame`).
    fn hit(&self, pos: egui::Pos2) -> Option<Control> {
        match self {
            // The confirm card's rows are *not* in this list. They are the
            // list of files the answer is about — nothing to click — and
            // reporting them as a control gave them a pointing hand and a
            // hover lift for an action that resolves to nothing
            // (`delightful-ui` §2: a wrong cursor reads as broken).
            OverlayGeom::Confirm(g) => g.action_at(pos).map(Control::Action),
            OverlayGeom::Conflict(g) | OverlayGeom::Bulk(g) => g
                .action_at(pos)
                .map(Control::Action)
                .or_else(|| {
                    g.apply_all
                        .filter(|r| r.contains(pos))
                        // The toggle sits one past the answers, so one index
                        // space covers every button on the card.
                        .map(|_| Control::Action(dialog::ConflictAction::ALL.len()))
                })
                .or_else(|| g.row_at(pos).map(Control::PanelRow)),
            OverlayGeom::Picker(_, rows) | OverlayGeom::Panel(_, rows, _) => rows
                .iter()
                .position(|r| r.contains(pos))
                .map(Control::PanelRow),
            OverlayGeom::Finder(geometry) => geometry.row_at(pos).map(Control::PanelRow),
            OverlayGeom::Search(geometry) => geometry.row_at(pos).map(Control::PanelRow),
            OverlayGeom::Mounts(geometry) => geometry.row_at(pos).map(Control::PanelRow),
            // The spot has two kinds of target on one card — nine permission
            // chips and the checksum's button — so it does its own hit test.
            OverlayGeom::Spot(geometry) => geometry.hit(pos),
        }
    }

    /// Where a control was drawn, for the ripple to start from.
    fn rect_of(&self, control: Control) -> Option<egui::Rect> {
        match (self, control) {
            (OverlayGeom::Confirm(g) | OverlayGeom::Conflict(g), Control::Action(i)) => {
                g.actions.get(i).copied().or(g.apply_all)
            }
            (OverlayGeom::Confirm(g) | OverlayGeom::Conflict(g), Control::PanelRow(i)) => {
                g.rows.get(i).copied()
            }
            (
                OverlayGeom::Picker(_, rows) | OverlayGeom::Panel(_, rows, _),
                Control::PanelRow(i),
            ) => rows.get(i).copied(),
            (OverlayGeom::Finder(geometry), Control::PanelRow(i)) => geometry.rows.get(i).copied(),
            (OverlayGeom::Search(geometry), Control::PanelRow(i)) => geometry.rows.get(i).copied(),
            (OverlayGeom::Mounts(geometry), Control::PanelRow(i)) => geometry.rows.get(i).copied(),
            (OverlayGeom::Spot(geometry), control) => geometry.rect_of(control),
            _ => None,
        }
    }
}

/// Where the primary button went down, and on what.
///
/// Held for two reasons, and they pull in opposite directions: a drag that
/// begins on **empty pane space** is a band select, and a drag that begins **on
/// a row** is a file drag (the next phase's internal DnD). Which of the two a
/// gesture is has to be decided at *press* time — by the time the pointer has
/// moved [`crate::mouse::DRAG_THRESHOLD`] it is over some other row and the
/// question can no longer be asked.
#[derive(Debug, Clone, Copy)]
struct PressStart {
    at: egui::Pos2,
    /// The press landed on a row of the list — reserved for DnD.
    on_row: bool,
    /// The press landed on the basket's chip, which drags the whole basket
    /// (PLAN §7.1).
    on_basket: bool,
    /// The press landed on a tab chip, which drags the tab out into a window
    /// (PLAN §2).
    on_tab: Option<usize>,
    /// The press landed inside the list pane, which is the only pane a band
    /// can be drawn in.
    in_list: bool,
    /// Whether the drag threshold has already been crossed, so the decision is
    /// made once rather than re-made every frame.
    dragging: bool,
}

/// One frame's worth of pointer state, read out of egui in a single pass.
///
/// Bundled because it is read once and used in six places, and because reading
/// it piecemeal is how two of those places end up disagreeing about whether the
/// button is down.
struct Pointer {
    at: Option<egui::Pos2>,
    down: bool,
    pressed: bool,
    released: bool,
    secondary: bool,
    middle: bool,
    /// The wheel this frame, in logical points.
    wheel: f32,
    shift: bool,
    /// `Ctrl` (or the platform's command key): toggle one row's selection.
    toggle: bool,
    /// `Alt`, which only a drag reads — it is the third of the three drop verbs
    /// (`crate::dnd::verb_for`).
    alt: bool,
}

/// Where this frame drew the things a click can land on.
///
/// A borrow of the frame's own locals rather than of `self`, so a `&mut self`
/// handler can still be given the geometry it needs.
struct Geom<'a> {
    layout: &'a ui::Layout,
    /// How many items fit in the list pane — what a half-page means, and what
    /// a palette row has to hand `run` when it dispatches a command.
    page: usize,
    /// The list pane's content box, and how far it has scrolled.
    list: egui::Rect,
    list_scroll: f32,
    /// `Some` when this directory is drawn as a grid: the tile geometry every
    /// hit test goes through (see [`crate::grid::pane_at`]).
    grid: Option<grid::Metrics>,
    parent: egui::Rect,
    parent_scroll: f32,
    crumbs: &'a [egui::Rect],
    overlay: &'a Option<OverlayGeom>,
    menu: &'a Option<menu::Geometry>,
    /// The selection basket's tray (PLAN §7.1).
    basket: &'a crate::basket::Geometry,
    tabs: usize,
}

/// Which piece of a path `c c` / `c d` / `c f` / `c n` copy (PLAN §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece {
    Path,
    Dirname,
    Filename,
    Stem,
}

/// egui's wheel unit, in this program's spelling.
fn wheel_unit(unit: egui::MouseWheelUnit) -> crate::mouse::WheelUnit {
    match unit {
        egui::MouseWheelUnit::Point => crate::mouse::WheelUnit::Point,
        egui::MouseWheelUnit::Line => crate::mouse::WheelUnit::Line,
        egui::MouseWheelUnit::Page => crate::mouse::WheelUnit::Page,
    }
}

/// How long after the last change the state file is written (PLAN §2).
///
/// Two seconds. Long enough that walking through five directories toggling
/// views is one write rather than five, short enough that anything you did more
/// than a moment ago is already on disk if the machine goes down. It is a
/// deadline, not a timer: one scheduled wake-up, and the resting state has
/// none.
const STATE_FLUSH: Duration = Duration::from_secs(2);

/// How many rows either side of the visible window take part in a FLIP.
///
/// Four. Enough that a row travelling in from just off the edge is already in
/// the animation when it arrives — otherwise it would pop into place at the top
/// of the pane rather than sliding in — and small enough that a re-sort of a
/// ten-thousand-file directory animates a couple of dozen things rather than
/// ten thousand.
const FLIP_MARGIN: usize = 4;

/// The state file's write-behind timer (PLAN §2).
///
/// df-core's [`StateStore`] knows *what* changed and refuses to write when
/// nothing has; this owns *when*. The two failures it sits between are real:
/// writing on every change is an atomic replace per keystroke of a `,` chord,
/// and writing only at quit loses the session to any crash. A deadline rather
/// than a timer, so an idle window with a pending write schedules exactly one
/// wake-up and then sleeps (PLAN §1).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct WriteBehind {
    due: Option<Instant>,
}

impl WriteBehind {
    /// Something changed. Arms the write, or leaves it alone when the store
    /// says there is nothing to save.
    ///
    /// The deadline is pushed out by each change rather than fixed at the
    /// first, which is what makes it a debounce: toggling five directories in
    /// four seconds is one write, not five.
    fn touch(&mut self, dirty: bool, now: Instant) {
        if dirty {
            self.due = Some(now + STATE_FLUSH);
        }
    }

    /// Is the write owed? Disarms itself when it says yes, so one arming is
    /// one write.
    fn ready(&mut self, now: Instant) -> bool {
        match self.due {
            Some(due) if now >= due => {
                self.due = None;
                true
            }
            _ => false,
        }
    }

    /// How long until the write, for the repaint deadline. `None` is the
    /// resting state: nothing pending, no frame owed.
    fn deadline(&self, now: Instant) -> Option<Duration> {
        Some(self.due?.saturating_duration_since(now))
    }

    /// Forget the pending write — the quit path, which is doing it now.
    fn disarm(&mut self) {
        self.due = None;
    }
}

/// `$HOME`, for shortening the paths a jump overlay lists.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Seconds since the epoch, which is the clock zoxide's frecency is scored
/// against. A clock before 1970 scores everything as ancient rather than
/// panicking, which is the right way for a bad clock to fail.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A path's last component, as a `String`.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// "file is" / "files are", for the one message that needs it.
fn plural_verb(n: usize) -> &'static str {
    if n == 1 {
        "file is"
    } else {
        "files are"
    }
}

/// How a session ended, and therefore whether the cwd-file is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quit {
    /// `q`, or closing the window: the shell wrapper follows you here.
    WriteCwd,
    /// `Q`: leave the shell where it was (PLAN §4.1).
    Silent,
}

/// The whole application.
pub struct App {
    /// `None` until `resumed` — winit only hands out a window once the platform
    /// is ready, and on Wayland that is not at construction time.
    gfx: Option<Gfx>,
    /// The handle worker threads take a clone of. Built in `main` before the
    /// window exists, so the cold-start ordering PLAN §6 asks for — workers
    /// started *before* the window — is what actually happens.
    waker: Waker,
    /// When the next frame is owed, if one is. `None` means "asleep until
    /// something happens" — the resting state.
    repaint_at: Option<Instant>,
    /// Consecutive frames the surface refused (`Gfx::present` returned false).
    /// Drives the retry back-off in `redraw`, and is reset by the first frame
    /// that lands.
    present_failures: u32,
    /// The off-thread stall detector (see [`crate::watchdog`]).
    watchdog: crate::watchdog::Watchdog,
    logged_first_frame: bool,
    /// One-shot marker for PLAN §6's cold-start measurement: the first frame
    /// that actually had rows in it. Separate from `logged_first_frame`,
    /// because the two numbers answer different questions — how long until
    /// there is a window, and how long until there is a *directory* — and in a
    /// cold cache they are not the same instant.
    logged_first_listing: bool,

    // ── The model ───────────────────────────────────────────────────────────
    config: Config,
    theme: Theme,
    palette: Palette,
    /// The live view settings: sort, linemode, hidden. Starts as the config's
    /// and is what the `,`, `m` and `.` bindings change.
    mgr: MgrConfig,
    /// The shuffle for `, r`. Held rather than redrawn per sort, so a random
    /// order stays put while it is scrolled.
    seed: u64,
    keymap: Registry,
    keys: KeymapState,
    context: ContextStack,
    scanner: Scanner,
    watcher: Watcher,
    /// The preview pane's whole state: its workers, what it is showing, and
    /// how far that has been scrolled (PLAN §6).
    preview: PreviewPane,
    /// Which pane the keyboard is in (PLAN §2.1). One field, read by the
    /// `when` predicates and by the paint — see [`crate::focus`].
    focus: Focus,
    /// Where the focus *treatment* is, which lags [`App::focus`] by
    /// [`crate::focus::FOCUS_FADE`] (PLAN §2.1). Kept beside it rather than
    /// inside it because focus is a decision and this is a picture of one.
    focus_fade: crate::focus::FocusFade,
    /// The transport, built the first time a playable file is hovered and kept
    /// after that (PLAN §4.3). `None` is the resting state of a session that
    /// has only ever looked at photographs: no cpal device, no decode thread.
    player: Option<Player>,
    /// ffmpeg's answer about the hovered file, off the paint thread.
    prober: Prober,
    /// The last few probes, so arrowing back onto a clip does not re-open it.
    ///
    /// A tiny ring rather than a cache with a policy: what it has to cover is a
    /// `↓ ↑` and a walk back up a directory, and past that the probe is cheaper
    /// than remembering.
    probes: Vec<(PathBuf, Option<TemporalInfo>)>,
    /// What a probe is in flight for, so a held `↓` asks once per file.
    probing: Option<PathBuf>,
    /// Whether the keystroke being routed came from key **repeat**.
    ///
    /// The shuttle ladder is the only thing in the program that cares — a held
    /// `l` is paced to one doubling per `SHUTTLE_HOLD_STEP` while discrete
    /// presses double instantly (PLAN §4.3) — so it is carried as one field
    /// beside the press rather than threaded through every command's signature.
    key_repeat: bool,
    tabs: Tabs,
    /// The file named on the command line, and the directory it is in, until
    /// the scan that will contain it has landed and the cursor is on it.
    ///
    /// `delightfile /path/to/file.txt` opens the *directory* and points at the
    /// file, and the directory read is asynchronous — so the name has to
    /// outlive the moment it was asked for. Cleared the instant it lands, or
    /// when the scan finishes without it (a file that was deleted between the
    /// shell and here).
    start_cursor: Option<(PathBuf, String)>,
    /// `--cwd-file`, written on a `q` quit (PLAN §3).
    cwd_file: Option<PathBuf>,
    quit: Option<Quit>,

    // ── Input ───────────────────────────────────────────────────────────────
    /// Keystrokes that arrived since the last frame. Queued rather than acted
    /// on in `window_event` because a command needs the pane geometry — a page
    /// is however many rows are on screen — and that is only known mid-frame.
    pending_keys: Vec<Press>,
    modifiers: ModifiersState,
    /// The bottom bar, when something is being typed into it.
    prompt: Option<Prompt>,
    /// Visual mode (`v` / `V`), while it is on.
    visual: Option<Visual>,
    /// The pointer's own state (PLAN §7.5): what the last click was, where the
    /// button went down, and the band it is dragging.
    clicks: crate::mouse::Clicks<Control>,
    press: Option<PressStart>,
    band: Option<select::Band>,
    /// The right-click menu, while it is up — or fading (see [`menu::Menu`]).
    menu: Option<menu::Menu>,
    /// The last `/` or `?`, so `n` and `N` have something to repeat.
    last_find: Option<(String, FindDirection)>,

    // ── Overlays ────────────────────────────────────────────────────────────
    /// The `~` / `F1` help browser's view state, while it is open.
    help: Option<Help>,
    /// The help browser's own `f` filter. Held here rather than in the prompt
    /// so that submitting the filter can close the bar without also throwing
    /// away what was typed into it.
    help_query: String,
    /// `Ctrl+p`, `z` and `Z` — the one fuzzy card, whichever of the three
    /// opened it (PLAN §4.4, §7.2). One field because they are one surface:
    /// two of them open at once is not a state that exists.
    finder: Option<Finder>,
    /// Which commands have been run from the palette this session, so an empty
    /// palette opens on what you did last. Session-local; see
    /// [`crate::finder::Mru`].
    mru: finder::Mru,
    /// zoxide's database, read once when `z` or `Z` first asks for it.
    ///
    /// Lazy because a session that never jumps should not pay to parse a
    /// database it will not look at, and cached because re-reading it on every
    /// keystroke of a `Z` query would be a file read per character.
    zoxide: Option<Vec<df_core::zoxide::ZoxideDir>>,
    /// `s` / `S`, while one is open (PLAN §7.2).
    search: Option<Search>,
    /// The preview owes the search cursor a scroll-to-line.
    ///
    /// A flag rather than a scroll every frame: `scroll_to` clamps against the
    /// height the *last* paint measured, so the first attempt after a result is
    /// highlighted usually lands short and has to be repeated once the preview
    /// has loaded — but repeating it unconditionally would be a window that
    /// never goes to sleep (PLAN §1).
    search_follow: bool,
    // ── Operations (PLAN §5) ────────────────────────────────────────────────
    /// The worker pool. Started before the window, like every other worker.
    engine: TaskEngine,
    /// The engine's transition stream, taken **once** at startup: `events()`
    /// replaces the channel every time it is called, so a second call would
    /// silently orphan the first receiver.
    task_events: crossbeam_channel::Receiver<TaskEvent>,
    ops: Vec<PendingOp>,
    /// The undo stack (PLAN §5).
    journal: Journal,
    /// What `y` / `x` filled and `p` will paste.
    clipboard: Clipboard,
    /// The one-at-a-time toast.
    toasts: Toasts,
    /// The modal card, when one is up. While it is, keys are matched against
    /// its context *alone* — see [`App::overlay_key`].
    dialog: Option<Dialog>,
    /// `O`'s opener chooser.
    picker: Option<Picker>,
    /// `w`'s task panel.
    panel: Option<TaskPanel>,
    /// `Tab`'s spot panel (PLAN §6).
    spot: Option<Spot>,
    /// The git front end, started the first time something asks it a question.
    ///
    /// Lazy because most of what this program does needs no git at all: a
    /// session spent in `~/Pictures` should not pay for a worker thread and a
    /// `git status` process that nothing was ever going to read.
    git: Option<df_core::git::Git>,
    /// An archive being listed off the event loop (PLAN §7.3), and what the
    /// listing is for.
    ///
    /// One at a time: opening a second archive while the first is still being
    /// read replaces the request, because there is one list pane and only the
    /// last thing asked for can land in it.
    archive_job: Option<PendingArchive>,
    /// The recursive-size walker, started the first time "what's big" is asked
    /// for. `None` is the resting state of a session that never asked: two
    /// worker threads and a cache bought for a feature nobody used is exactly
    /// the cost PLAN §1's idle discipline is about.
    du: Option<df_core::du::DuScanner>,
    /// PLAN §7.3's "what's big" mode, while it is on.
    usage: Option<crate::usage::Usage>,
    /// The udisks2 worker, started the first time `M` is pressed. `None` is the
    /// resting state of a session that never asked about disks: no thread, no
    /// system-bus connection.
    udisks: Option<crate::mounts::Mounts>,
    /// The disks card, while it is open (PLAN §7.4).
    mounts: Option<crate::mounts::Card>,
    /// Files collected across directories (PLAN §7.1). Session-lived: see
    /// [`crate::basket`].
    basket: crate::basket::Basket,
    /// Whether the tray is expanded. The *chip* is always there while the
    /// basket has anything in it — that is what makes the basket visible enough
    /// to earn its place ahead of the system clipboard in `p`'s ladder.
    basket_open: bool,
    /// The first row the expanded tray draws.
    basket_first: usize,
    /// The archive entry the preview card is showing, and its text if it had
    /// any: `(archive, inner path, body)`.
    archive_preview: Option<(PathBuf, String, Option<String>)>,

    // ── Remote services (PLAN §7.6) ─────────────────────────────────────────
    /// The vfs, started the first time `g 1` asks for a service.
    ///
    /// `None` is the resting state of a session that never went anywhere
    /// remote: no worker thread, no `ssh`, no `vfs.toml` read — the same
    /// laziness `git`, `du` and `udisks` get, and for the same reason.
    /// [`Arc`] because a download job outlives the frame that spawned it.
    vfs: Option<Arc<df_core::vfs::Vfs>>,
    /// Every local file a remote session has produced, and the one place they
    /// are removed from. Swept on quit.
    temps: crate::remote::Temps,
    /// Remote operations running on the pool, and what each owes the UI when it
    /// lands. Plural because a download and a rename can be in flight at once.
    remote_ops: Vec<PendingRemote>,
    /// The preview card's state for the remote row under the cursor.
    remote_preview: Option<RemotePreview>,
    /// The row the cursor is resting on and since when — the preview's debounce
    /// (see [`crate::remote::PREVIEW_DEBOUNCE`]). A held `↓` through a hundred
    /// remote rows has to cost nothing at all.
    remote_hover: Option<(String, Instant)>,
    /// Services that have answered at least once this session.
    ///
    /// What the "Connecting to …" sticky toast is keyed on: the *first* listing
    /// of a service is a TCP connect, a key exchange and possibly a whole
    /// `ProxyJump` chain, and it is the only wait the user needs telling about.
    /// Every listing after it is one round trip on a connection that is already
    /// up, and a toast for that would be noise.
    remote_connected: HashSet<String>,

    // ── The trash (PLAN §7.4) ───────────────────────────────────────────────
    /// What the linemode column says in the trash view: each row's original
    /// directory, by name. Rebuilt with the listing, so it cannot describe rows
    /// that are no longer there.
    trash_notes: HashMap<String, String>,
    /// The repository the current directory is in, recomputed only when the
    /// directory changes.
    ///
    /// This field is what keeps the laziness above honest now that the *rows*
    /// want git too. The dots are wanted on every frame, and asking
    /// [`df_core::git::Git`] for them would start the worker in every session —
    /// including the ones spent entirely outside a repository, which is most of
    /// them. A walk up for `.git` is one `stat` per level, so doing it once per
    /// navigation and caching the `None` costs nothing and means a session in
    /// `~/Pictures` still never spawns a thread.
    repo: Option<PathBuf>,
    /// Where the cursor row was last drawn: what a rename popup and the opener
    /// picker anchor themselves to (PLAN §4.2, §6).
    cursor_rect: egui::Rect,
    /// The breadcrumb (PLAN §2), cached for the directory it describes.
    ///
    /// Rebuilt on a directory change rather than per frame: the segments are a
    /// handful of `String`s and the branch is two file reads
    /// (`df_core::git::repo`, which never spawns anything), but both are pure
    /// waste on the sixty frames a scroll costs.
    path_bar: (PathBuf, Vec<chrome::Crumb>, Option<String>),

    /// The which-key card's timing (PLAN §4).
    which: WhichKey,
    /// What the card lists: `(keys, description)` in df-core's declaration
    /// order. Kept after the chord resolves so the card has something to draw
    /// while it fades.
    which_rows: Vec<(String, String)>,

    // ── Painting ────────────────────────────────────────────────────────────
    /// Hover/press amounts for every row on screen (PLAN §8).
    hovers: Hovers<Control>,
    /// The cursor's own "instant in, animated out" track, keyed by row, so a
    /// cursor moved by the keyboard leaves a trail exactly as the pointer does.
    cursor_glow: Hovers<usize>,
    ripples: Ripples<Control>,
    /// Whether a patched font was found and the real icons can be drawn.
    nerd: bool,
    /// Which directories are drawn as grids, and the rest of the per-directory
    /// memory (PLAN §2). Loaded at startup, written back on a debounce — the
    /// store tracks *what* changed and this file owns *when* it is written.
    state: StateStore,
    /// When the state file is due to be written. See [`WriteBehind`].
    state_due: WriteBehind,
    /// The grid's thumbnail workers, started the first time a directory is
    /// drawn as a grid and kept after that.
    ///
    /// `None` is the resting state of a session that has only ever used the
    /// list: no threads, no textures, nothing decoded.
    thumbs: Option<Thumbs>,
    /// How many entries share one row of the list pane: 1 in the list, the
    /// grid's column count in the grid.
    ///
    /// Held on the app rather than passed around because the two places that
    /// need it — the cursor commands in [`App::run`] and the wheel — run
    /// *before* the frame that measures the pane, so there is nothing to pass
    /// yet. Set at the top of every frame, so it is at most one resize stale
    /// and a resize cannot happen between a key and the frame it is routed in.
    columns: usize,
    /// How tall one row of the list pane is: [`ui::ROW_HEIGHT`] in the list, a
    /// whole row of tiles in the grid. Held for the same reason `columns` is.
    pane_step: f32,
    /// A re-sort in flight (PLAN §2's FLIP).
    flip: Option<Flip>,
    /// Where the visible rows were the moment a reordering command ran, waiting
    /// for the next frame's layout to animate against.
    ///
    /// Captured in [`App::run`] rather than in the frame, because by the time
    /// the frame runs the sort has already happened and "where things were" is
    /// gone.
    flip_before: Option<Snapshot>,
    /// Where the visible rows were drawn last frame — the thing `flip_before`
    /// is a copy of.
    last_layout: Snapshot,

    // ── Drag and drop (PLAN §7.1) ───────────────────────────────────────────
    /// The compositor-side data device: drags out, drops in. `None` when the
    /// protocol is not there to be had (see [`crate::wayland`]), and every use
    /// of it is written so that is simply a session without cross-application
    /// drags rather than a session with a hole in it.
    data_device: Option<crate::wayland::DataDevice>,
    /// The files in the hand, while there are any.
    drag: Option<Drag>,
    /// A tab chip being pulled out of the strip (PLAN §2's "drag a tab out to
    /// spawn a window"). Separate from [`Drag`] because it carries a *tab*, not
    /// files: nothing about targets, verbs, spring-open or the Wayland hand-off
    /// applies to it, and the one thing they share — the ghost and its flight
    /// home — is shared by using the same painter and the same
    /// [`dnd::SpringBack`].
    tab_drag: Option<TabDrag>,
    /// Windows this one has opened, so none of them becomes a zombie. See
    /// [`crate::window`] for why a window is a process.
    windows: crate::window::Windows,
    /// The ghost's flight home after a cancelled drag. Outlives the drag it
    /// belongs to — that is the whole point of it.
    spring_back: Option<SpringHome>,
    /// Highlight amounts for drop targets: one more "instant in, animated out"
    /// track (`delightful-ui` §3), so a ring snaps on as the drag arrives and
    /// fades as it leaves.
    targets: Hovers<dnd::Target>,
    /// A drag from *another* application, while it is over the window.
    incoming: Option<Incoming>,
    /// The last frame's drop geometry. An external drop arrives from the
    /// wayland thread between frames and has to be resolved against the frame
    /// the user was actually looking at when they let go.
    zones: Option<dnd::Zones>,
}

/// What one frame of a live drag hands the painter.
struct DragFrame {
    at: egui::Pos2,
    verb: dnd::Verb,
    /// The target under the pointer, and `None` when there is none *or* when
    /// the one there is would refuse the drop — an inert target must look
    /// inert (`delightful-ui` §6).
    target: Option<dnd::Target>,
}

/// The drag in progress: what is in the hand and where it came from.
struct Drag {
    /// What will be moved, copied or linked. Fixed at press time — a drag that
    /// re-read the selection as it went would act on rows the hand had scrolled
    /// past rather than on the ones it picked up.
    paths: Vec<PathBuf>,
    /// The name on the top card. The *grabbed* row's, not the first selected
    /// one's: the card has to show what the hand actually took hold of.
    label: String,
    /// The glyph and colour beside it, from the same table the rows use.
    icon: crate::icons::Icon,
    /// Where the ghost springs back to — the middle of the row it came off.
    home: egui::Pos2,
    at: egui::Pos2,
    /// The hold-to-open timer, aimed at whatever the drag is over.
    spring: dnd::SpringOpen,
    /// The last frame's instant, so the auto-scroll is in rows per *second*
    /// rather than rows per frame — a drag must not travel further on a fast
    /// machine.
    last: Instant,
    /// The compositor has taken the pointer: this drag is now somebody else's
    /// problem until it comes back as a `DragEnded`.
    handed_off: bool,
}

/// A tab chip on its way out of the strip (PLAN §2).
///
/// The gesture is decided by [`crate::window::armed`] from `from` and `at`, so
/// what the ghost advertises and what letting go does are the same three facts
/// — a ghost that says "new window" and a release that springs back would be
/// the drag lying about itself.
struct TabDrag {
    /// Which tab, by index. Re-read at the moment of the detach rather than
    /// held as a `Tab`, because the strip can change under a drag (a scan
    /// landing, a `}`), and an index that no longer exists is a detach that
    /// does nothing rather than one that takes the wrong tab.
    tab: usize,
    /// Where the button went down — the threshold is measured from here.
    from: egui::Pos2,
    at: egui::Pos2,
    /// The middle of the chip, which is where the ghost flies home to.
    home: egui::Pos2,
    /// The tab's title, on the face of the ghost.
    label: String,
    icon: crate::icons::Icon,
}

/// A cancelled drag on its way back to the row it came off.
///
/// The card keeps its face: what flies home has to be recognisably the thing
/// that was picked up, or the animation reads as a new object appearing rather
/// than as the drag being undone.
struct SpringHome {
    spring: dnd::SpringBack,
    label: String,
    icon: crate::icons::Icon,
}

/// A drag from another application, over our window.
struct Incoming {
    at: egui::Pos2,
    /// Whether it is our own drag come back through the compositor — see
    /// [`crate::dnd::SELF_MIME`].
    ours: bool,
}

impl App {
    /// Build the model and start its workers. Called before the window exists.
    pub fn new(waker: Waker, args: crate::cli::Args) -> App {
        let loaded = df_core::config::load();
        for warning in &loaded.warnings {
            log::warn!("{warning}");
        }
        let config = loaded.config;
        let theme = loaded.theme;

        let mut keymap = Registry::defaults();
        if let Some(dir) = df_core::config::config_dir() {
            // Bookmarks first, so a `keymap.toml` that rebinds a `g` chord wins
            // over the `[goto]` table it would otherwise collide with.
            for warning in keymap.apply_bookmarks(&config.goto, &dir.join("delightfile.toml")) {
                log::warn!("{warning}");
            }
            for warning in keymap.apply_overrides_from_dir(&dir) {
                log::warn!("{warning}");
            }
        }

        // One bell, four ropes: the handles differ only in the name they log
        // under `DF_FRAME_LOG` (see [`Waker::named`]), because "who woke us at
        // rest?" is the only question the idle-cost audit cannot answer from
        // the outside.
        let bell = |source: &'static str| -> df_core::fs::Notifier {
            let waker = waker.named(source);
            Arc::new(move || waker.wake())
        };
        let scanner = Scanner::start(bell("scanner"));
        let watcher = Watcher::start(bell("watcher"));
        // Before the window, with the scanner (PLAN §6's cold-start ordering:
        // "decode workers started **before** the window").
        let mut preview = PreviewPane::start(bell("preview"));
        // Before the window as well, and for the same reason: the first thing
        // a probe is asked about is whatever file the cursor opens on.
        let prober = Prober::start(bell("prober"));

        // ── What that ordering actually buys, measured (PLAN §6) ────────────
        // Debug build (`opt-level = 1`), warm page cache, five runs each,
        // timing the two `log::info!` markers below against the process's own
        // launch instant: "window mapped" in `redraw`, and "first listing"
        // the first frame that has rows in it.
        //
        //   ~40 entries  (this repo)         window 224–241 ms, listing same frame
        //   ~800 entries (a Work subtree)    window 230–238 ms, listing same frame
        //   9975 entries (a flat thumb dir)  window 462–510 ms, listing same frame
        //
        // The number that matters is not either column — it is that they are
        // the *same* column. The directory read finishes while wgpu is still
        // negotiating an adapter, so the first frame the compositor ever shows
        // already has the listing in it. There is no empty-pane flash to
        // crossfade away from at any directory size tested, which is the whole
        // reason these four `start` calls sit above `init_gfx` rather than in
        // it. Move any of them after the window and that column splits.
        //
        // Everything else a session needs — git, du, thumbnails, udisks, the
        // vfs, search — is started lazily on first use, deliberately: none of
        // them has anything to say about frame one, and four threads that
        // cannot contribute to the first frame are four threads competing with
        // the ones that can.

        // Before the window as well (PLAN §6's cold-start ordering), and wired
        // to the same bell every other worker rings.
        let engine = TaskEngine::new(&config.tasks);
        {
            let waker = waker.named("tasks");
            engine.set_notifier(Box::new(move || waker.wake()));
        }
        let task_events = engine.events();

        let mgr = config.mgr.clone();
        let seed = 0;
        let sort = sort_options(&mgr, seed);
        // A directory *peek* is listed the way entering it would list it.
        preview.set_sort(sort);
        let now = Instant::now();
        let (start, focus) = start_directory(args.start.as_deref());
        let mut tab = Tab::open(start, &mgr, sort, &scanner, now);
        // **Opening on a file puts the cursor on it once its row arrives.**
        // Trying only here was the bug: `Tab::open` *queues* the scan, so at
        // this line the listing is empty and `cursor_to_name` has nothing to
        // find — `delightfile /path/to/file.txt` left the cursor on row 0 and
        // looked like it had ignored the argument. The name is remembered
        // instead and retried as each batch lands (see
        // [`App::place_start_cursor`]). The attempt is still made now as well,
        // in case some future path has the entries in hand already.
        let start_cursor = focus.and_then(|name| {
            place_start_cursor(&mut tab.cwd.dir, &name)
                .then(|| (tab.cwd.path().to_path_buf(), name))
        });
        watcher.watch(tab.watched());

        App {
            gfx: None,
            waker,
            repaint_at: None,
            present_failures: 0,
            watchdog: crate::watchdog::Watchdog::start(),
            logged_first_frame: false,
            logged_first_listing: false,
            palette: Palette::from_theme(&theme),
            config,
            theme,
            mgr,
            seed,
            keymap,
            keys: KeymapState::new(),
            context: ContextStack::browser(),
            scanner,
            watcher,
            preview,
            focus: Focus::default(),
            focus_fade: crate::focus::FocusFade::new(),
            player: None,
            prober,
            probes: Vec::new(),
            probing: None,
            key_repeat: false,
            tabs: Tabs::new(tab),
            start_cursor,
            cwd_file: args.cwd_file,
            quit: None,
            engine,
            task_events,
            ops: Vec::new(),
            journal: Journal::default(),
            clipboard: Clipboard::default(),
            toasts: Toasts::new(),
            dialog: None,
            picker: None,
            panel: None,
            spot: None,
            git: None,
            repo: None,
            archive_job: None,
            archive_preview: None,
            vfs: None,
            temps: crate::remote::Temps::default(),
            remote_ops: Vec::new(),
            remote_preview: None,
            remote_hover: None,
            remote_connected: HashSet::new(),
            trash_notes: HashMap::new(),
            du: None,
            usage: None,
            udisks: None,
            mounts: None,
            basket: crate::basket::Basket::default(),
            basket_open: false,
            basket_first: 0,
            cursor_rect: egui::Rect::ZERO,
            path_bar: (PathBuf::new(), Vec::new(), None),
            pending_keys: Vec::new(),
            modifiers: ModifiersState::empty(),
            prompt: None,
            visual: None,
            clicks: crate::mouse::Clicks::new(),
            press: None,
            band: None,
            menu: None,
            last_find: None,
            help: None,
            help_query: String::new(),
            finder: None,
            mru: finder::Mru::default(),
            zoxide: None,
            search: None,
            search_follow: false,
            which: WhichKey::new(),
            which_rows: Vec::new(),
            hovers: Hovers::new(),
            cursor_glow: Hovers::new(),
            ripples: Ripples::new(),
            nerd: false,
            // Read before the window, like every other startup read: the very
            // first frame has to know whether the directory it is opening is a
            // grid, or it would draw a list and then swap under the eye.
            state: StateStore::load(),
            state_due: WriteBehind::default(),
            thumbs: None,
            columns: 1,
            pane_step: ui::ROW_HEIGHT,
            flip: None,
            flip_before: None,
            last_layout: Snapshot::new(),
            data_device: None,
            drag: None,
            tab_drag: None,
            windows: crate::window::Windows::default(),
            spring_back: None,
            targets: Hovers::new(),
            incoming: None,
            zones: None,
        }
    }

    fn sort(&self) -> SortOptions {
        sort_options(&self.mgr, self.seed)
    }

    /// The tab on screen. Every command in this file acts on this one — a tab
    /// you are not looking at is a directory nobody asked about.
    fn tab(&self) -> &Tab {
        self.tabs.active()
    }

    /// The listing the cursor is in, which is what most commands mean by "the
    /// directory".
    fn dir(&mut self) -> &mut df_core::fs::DirState {
        &mut self.tabs.active_mut().cwd.dir
    }

    fn init_gfx(&mut self, event_loop: &ActiveEventLoop) -> Result<(), GfxError> {
        use winit::platform::wayland::WindowAttributesExtWayland;

        let attrs = Window::default_attributes()
            .with_title("delightfile")
            .with_inner_size(winit::dpi::LogicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1))
            .with_name(APP_ID, APP_ID);
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| GfxError(format!("create window: {e}")))?,
        );
        let gfx = Gfx::new(window)?;

        // Before the first frame, so no row is ever drawn with the wrong face.
        self.nerd = crate::icons::install(&gfx.egui_ctx);

        // egui can decide it needs a frame from a thread that is not this one —
        // a loading spinner, an animation driven by a background value. Without
        // a callback here that request lands nowhere, because nothing is
        // polling the context. Routing it through the same bell keeps one
        // wakeup path (PLAN §1). Only immediate requests ring it: a *delayed*
        // one is already carried by `repaint_delay` in `redraw`, and waking now
        // for a frame wanted later is how a wait turns into a poll.
        let waker = self.waker.named("egui");
        gfx.egui_ctx.set_request_repaint_callback(move |info| {
            if info.delay.is_zero() {
                waker.wake();
            }
        });

        // The data device needs the window's own surface, so it cannot be
        // started with the other workers before the window exists (PLAN §1's
        // ordering rule bends exactly this far and no further). A session
        // without one is a session with no cross-application drags and
        // everything else intact.
        self.data_device =
            Self::start_data_device(event_loop, &gfx.window, self.waker.named("wayland"));
        if self.data_device.is_none() {
            log::info!("no wayland data device — drag out and drop in are off");
        }

        self.gfx = Some(gfx);
        Ok(())
    }

    /// Adopt winit's Wayland connection for [`crate::wayland`].
    ///
    /// `None` on X11, on a compositor without the protocol, or when the handles
    /// cannot be had — all of which are "this desktop does not do that", not
    /// errors.
    fn start_data_device(
        event_loop: &ActiveEventLoop,
        window: &Window,
        waker: Waker,
    ) -> Option<crate::wayland::DataDevice> {
        use winit::raw_window_handle::{
            HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
        };
        let RawDisplayHandle::Wayland(display) = event_loop.display_handle().ok()?.as_raw() else {
            return None;
        };
        let RawWindowHandle::Wayland(surface) = window.window_handle().ok()?.as_raw() else {
            return None;
        };
        // SAFETY: both handles describe objects winit owns and keeps alive for
        // the lifetime of the window, and the device is dropped in `finish`
        // before the window is. The workspace warns on `unsafe_code`; this is
        // the one call site in df-app outside `crate::wayland`, and it is here
        // rather than inside that module because this is where the two handles
        // — and the promise about their lifetime — actually come from.
        #[allow(unsafe_code)]
        unsafe {
            crate::wayland::DataDevice::start(display.display, surface.surface, waker)
        }
    }

    /// Drain whatever the workers finished. Returns true when something changed
    /// and a frame is owed.
    fn poll_workers(&mut self) -> bool {
        let now = Instant::now();
        // The preview's own two workers. The context is where decoded pixels
        // become a texture, which is the one part of the preview that has to
        // happen on the thread egui lives on.
        let ctx = self.gfx.as_ref().map(|g| g.egui_ctx.clone());
        let mut changed = self.preview.poll(ctx.as_ref(), now);
        // ffmpeg's answers about hovered files. Kept as a small ring so a
        // `↓ ↑` does not re-open the clip, and applied by the next frame's
        // `sync_playback` — which is the one place that decides what to mount.
        let probed: Vec<crate::playback::Probed> = self.prober.drain().collect();
        for result in probed {
            changed = true;
            if self.probing.as_deref() == Some(result.path.as_path()) {
                self.probing = None;
            }
            self.probes.retain(|(path, _)| *path != result.path);
            self.probes.push((result.path, result.info));
            // Four is a `↓ ↑` and a step back up out of a directory. Past that
            // the probe is cheaper than the memory of it.
            while self.probes.len() > PROBE_MEMORY {
                self.probes.remove(0);
            }
        }
        // The search's reader thread (PLAN §7.2) and the grid's tile workers
        // (PLAN §2), both of which wake the loop the same way every other
        // worker does.
        if let Some(search) = &mut self.search {
            if search.poll() {
                changed = true;
                // A result arriving where the cursor is means the preview owes
                // it a scroll to the line it matched on.
                self.search_follow = true;
            }
        }
        if let Some(thumbs) = &mut self.thumbs {
            if thumbs.poll(ctx.as_ref()) {
                changed = true;
            }
        }
        for update in self.scanner.drain() {
            // Every tab, not only the active one: a tab opened a moment ago is
            // still loading behind the strip.
            if self.tabs.apply(&update) {
                changed = true;
            }
        }
        // The file named on the command line, whose row may only just have
        // arrived. Same shape as the parent's marker below, and for the same
        // reason: a cursor that has to land on a name cannot land before the
        // name is in the listing.
        if changed {
            self.place_start_cursor();
        }
        // The parent's marker follows the path, and the row it belongs on may
        // only just have arrived in a batch — *unless* the keyboard is in that
        // pane, where the marker is a cursor somebody is steering and a scan
        // update must not yank it back to the directory we are inside.
        if changed && self.focus != Focus::Parent {
            for tab in self.tabs.iter_mut() {
                tab.sync_parent_cursor();
            }
        }

        // The task engine. Every event is a repaint — a progress tick moves the
        // `w` panel's bar, and a terminal one owes somebody a toast. Collected
        // first because handling one takes `&mut self`.
        let events: Vec<TaskEvent> = self.task_events.try_iter().collect();
        for event in events {
            changed = true;
            self.task_event(event, now);
        }

        if self.sync_spot() {
            changed = true;
        }
        // The archive listing that `→` asked for, if it has landed (PLAN §7.3).
        if self.poll_archive(now) {
            changed = true;
        }
        // The card's body, read once per entry the cursor stops on.
        if self.sync_archive_preview() {
            changed = true;
        }
        // Whatever the remote services have listed (PLAN §7.6), and whatever
        // their operations have finished.
        if self.poll_vfs(now) {
            changed = true;
        }
        if self.poll_remote_ops(now) {
            changed = true;
        }
        // …and the remote preview card's body, once the cursor has rested on a
        // row long enough to be worth a round trip for.
        if self.sync_remote_preview(now) {
            changed = true;
        }
        // The recursive-size walk's running totals (PLAN §7.3).
        if self.poll_usage(now) {
            changed = true;
        }
        // Whatever udisks2 has said (PLAN §7.4).
        if self.poll_mounts(now) {
            changed = true;
        }

        for event in self.watcher.drain() {
            changed = true;
            match event {
                WatchEvent::Changed(dir) => self.rescan(&dir, now),
                // The directory we are in stopped existing. Walking up to the
                // nearest ancestor that still does is what a person would do
                // by hand, and leaving the pane showing a listing of a deleted
                // directory is the alternative.
                WatchEvent::Gone(dir) if dir == self.tab().cwd.path() => {
                    let up = nearest_existing(&dir);
                    log::info!("{} is gone; moving to {}", dir.display(), up.display());
                    self.navigate(up, now);
                }
                WatchEvent::Gone(_) => self.refresh_all(now),
                // Events were lost, so nothing is known about what changed.
                WatchEvent::Overflow => self.refresh_all(now),
            }
        }
        changed
    }

    /// Retry the start-up cursor against whatever has arrived.
    ///
    /// Gives up for good the moment the user has navigated somewhere else —
    /// a cursor placement asked for at startup must never fight a person who
    /// has already moved on.
    fn place_start_cursor(&mut self) {
        let Some((dir, name)) = self.start_cursor.clone() else {
            return;
        };
        let tab = self.tabs.active_mut();
        if tab.cwd.path() != dir {
            self.start_cursor = None;
            return;
        }
        if !place_start_cursor(&mut tab.cwd.dir, &name) {
            self.start_cursor = None;
        }
    }

    fn rescan(&mut self, dir: &Path, now: Instant) {
        // **A listing is only ever re-read from a real directory.** `dir` comes
        // from watcher events, from an operation's touched paths and from a
        // finished remote job, and one of those used to be able to carry an
        // `sftp://…` URL — which `begin_scan` would hand to `read_dir`, failing,
        // and replacing the remote rows on screen with an error. Loud in the
        // log rather than a `debug_assert`: this is a wrong path, not a reason
        // to take a running session down.
        if !scannable(dir) {
            log::error!(
                "rescan asked for {}, which is not a directory on this machine",
                dir.display()
            );
            return;
        }
        self.git_touched(dir);
        let scanner = &self.scanner;
        let tab = self.tabs.active_mut();
        if dir == tab.cwd.path() {
            tab.cwd.begin_scan(scanner, now);
        }
        if let Some(parent) = &mut tab.parent {
            if dir == parent.path() {
                parent.begin_scan(scanner, now);
            }
        }
    }

    fn refresh_all(&mut self, now: Instant) {
        if let Some(root) = self.repo.clone() {
            self.git().refresh(&root);
        }
        // A remote pane refreshes by forgetting: the cache is the only reason
        // it did not go back to the server, so a deliberate refresh drops it
        // and re-lists where you are (PLAN §7.6).
        if self.tab().remote.is_some() {
            let (mgr, sort) = (self.mgr.clone(), self.sort());
            if let Some(session) = &mut self.tabs.active_mut().remote {
                session.forget_all();
            }
            if let Some(at) = self.tabs.active_mut().refresh_remote(&mgr, sort, now) {
                self.scan_remote(at, now);
            }
            return;
        }
        if self.tab().trash.is_some() {
            self.refresh_trash(now);
            return;
        }
        let (mgr, sort) = (self.mgr.clone(), self.sort());
        self.tabs
            .active_mut()
            .rescan_all(&mgr, sort, &self.scanner, now);
        self.rewatch();
    }

    fn navigate(&mut self, path: PathBuf, now: Instant) {
        // The two virtual locations are addressed by URL, so *every* door into
        // them — a `g` bookmark, a breadcrumb click, the palette, a `z` jump,
        // an `--cwd-file` argument — arrives here and is routed in one place.
        // A URL that reached the scanner would be a failed read of a directory
        // that does not exist.
        if let Some(at) = crate::remote::at_of(&path) {
            self.enter_remote(at, now);
            return;
        }
        if path == Path::new(crate::trashview::URL) {
            self.open_trash(now);
            return;
        }
        let (mgr, sort) = (self.mgr.clone(), self.sort());
        self.tabs
            .active_mut()
            .navigate(path, &mgr, sort, &self.scanner, now);
        // Moving is leaving: a visual run is anchored to a row in the directory
        // that is no longer on screen.
        self.visual = None;
        self.rewatch();
    }

    /// Point the watcher at the active tab's directories (PLAN §2: the list and
    /// its parent). Called after anything that changes which those are —
    /// including a tab switch, which is the whole reason this is a function.
    fn rewatch(&mut self) {
        self.watcher.watch(self.tabs.active().watched());
    }

    // ── Operations (PLAN §5) ────────────────────────────────────────────────

    /// What an operation acts on: the selection, or — when there is none — the
    /// row under the cursor. yazi's rule, and the one every file manager has.
    fn targets(&self) -> Vec<PathBuf> {
        let dir = &self.tab().cwd.dir;
        let selected = dir.selected_paths();
        if !selected.is_empty() {
            return selected;
        }
        dir.cursor_entry()
            .map(|entry| vec![entry.path.clone()])
            .unwrap_or_default()
    }

    fn cwd(&self) -> PathBuf {
        self.tab().cwd.path().to_path_buf()
    }

    /// The directories an operation over `paths` could change: where they are
    /// now, and where they are going.
    fn affected(paths: &[PathBuf], dest: Option<&Path>) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = paths
            .iter()
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .collect();
        if let Some(dest) = dest {
            dirs.push(dest.to_path_buf());
        }
        dirs.sort();
        dirs.dedup();
        dirs
    }

    /// One task transition. The stream is for *reacting*; the panel renders
    /// from [`TaskEngine::snapshot`], never from this.
    fn task_event(&mut self, event: TaskEvent, now: Instant) {
        match event.state {
            TaskState::Done | TaskState::Cancelled => {
                self.finish_op(event.id, now);
                // A remote job's result is in its slot, not in the event, so
                // the slot is read *before* the op is forgotten — and forgotten
                // it is, because a job cancelled before a worker picked it up
                // never fills one and would otherwise sit here for ever.
                self.poll_remote_ops(now);
                self.remote_ops.retain(|op| op.id != event.id);
            }
            // A failure is not always the end — a transient one is republished
            // as `Failed { retries }` and then runs again — so the op stays
            // pending and only the message goes out now.
            TaskState::Failed { ref error, .. } if self.ops.iter().any(|op| op.id == event.id) => {
                self.toasts.error(format!("{}: {error}", event.name), now);
            }
            _ => {}
        }
    }

    /// A job is over: journal what it did, say so, and re-read what it touched.
    fn finish_op(&mut self, id: TaskId, now: Instant) {
        let Some(index) = self.ops.iter().position(|op| op.id == id) else {
            return;
        };
        let op = self.ops.remove(index);
        let outcome = match op.slot.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(mut outcome) = outcome {
            for (path, error) in &outcome.errors {
                log::warn!("{}: {error}", path.display());
            }
            let (message, kind) = op_toast(&outcome);
            if let Some(record) = outcome.record.take() {
                self.journal.record(record);
            }
            self.toasts.show(message, kind, now);
        }
        for dir in &op.dirs {
            self.rescan(dir, now);
        }
    }

    /// Queue a job and remember where to look for its result.
    fn track(&mut self, id: TaskId, slot: Outcome, dirs: Vec<PathBuf>) {
        self.ops.push(PendingOp { id, slot, dirs });
    }

    // ── Archives as directories (PLAN §7.3) ─────────────────────────────────

    /// Read an archive off the event loop, and then browse or unpack it.
    ///
    /// A job rather than an inline call because listing is a read of the whole
    /// file: a 400 MB `.tar.zst` is streamed through `zstd` and parsed block by
    /// block, and doing that between two frames is `→` freezing the window.
    fn ask_archive(&mut self, path: PathBuf, intent: ArchiveIntent) {
        // One list pane, so one request: opening a second archive while the
        // first is still being read cancels it, because only the last thing
        // asked for can land.
        if let Some(pending) = self.archive_job.take() {
            self.engine.cancel(pending.id);
        }
        let slot: Arc<std::sync::Mutex<Option<std::result::Result<_, String>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let job_slot = Arc::clone(&slot);
        let job_path = path.clone();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let job = FnJob::new(format!("Read {name}"), Lane::Micro, move |_ctx| {
            let result = df_core::archive::list(&job_path).map_err(|e| e.to_string());
            match job_slot.lock() {
                Ok(mut guard) => *guard = Some(result),
                Err(poisoned) => *poisoned.into_inner() = Some(result),
            }
            Ok(())
        });
        let id = self.engine.spawn(job);
        self.archive_job = Some(PendingArchive {
            id,
            path,
            intent,
            slot,
        });
    }

    /// Has the listing landed? Called once a frame, and a hash-free early
    /// return when nothing is in flight (PLAN §1).
    fn poll_archive(&mut self, now: Instant) -> bool {
        let Some(pending) = self.archive_job.as_ref() else {
            return false;
        };
        let landed = match pending.slot.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(result) = landed else {
            return false;
        };
        let Some(pending) = self.archive_job.take() else {
            return false;
        };
        let tree = match result {
            Ok(tree) => Arc::new(tree),
            Err(message) => {
                self.toasts.error(message, now);
                return true;
            }
        };
        match pending.intent {
            ArchiveIntent::Browse => self.enter_archive(pending.path, tree, now),
            ArchiveIntent::ExtractHere | ArchiveIntent::ExtractSubfolder => {
                let into = pending
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.cwd());
                let dest = self.extract_dest(
                    &pending.path,
                    into,
                    pending.intent == ArchiveIntent::ExtractSubfolder,
                    now,
                );
                if let Some(dest) = dest {
                    self.spawn_extract(&tree, &[], dest, now);
                }
            }
        }
        true
    }

    /// Put the tab inside the archive.
    fn enter_archive(
        &mut self,
        path: PathBuf,
        tree: Arc<df_core::archive::ArchiveTree>,
        now: Instant,
    ) {
        // Everything worth warning about, in one notice rather than three: the
        // listing is about to appear and a stack of toasts over it would be
        // read as an error (PLAN §5's one-at-a-time rule).
        let mut warnings: Vec<String> = Vec::new();
        if tree.truncated() {
            warnings.push("the listing was cut short".to_string());
        }
        if tree.unsafe_count() > 0 {
            warnings.push(format!(
                "{} cannot be extracted safely",
                plural(tree.unsafe_count(), "entry", "entries")
            ));
        }
        if tree.has_encrypted() {
            warnings.push("some entries are encrypted".to_string());
        }

        let browse = crate::archive::Browse { path, tree };
        let (mgr, sort) = (self.mgr.clone(), self.sort());
        self.tabs
            .active_mut()
            .open_archive(browse, &mgr, sort, &self.scanner, now);
        // Leaving is leaving, even into an archive: a visual run is anchored to
        // a row in a listing that is no longer on screen.
        self.visual = None;
        self.focus = Focus::List;
        self.rewatch();
        self.archive_preview = None;
        if !warnings.is_empty() {
            self.toasts.notice(warnings.join("; "), now);
        }
    }

    /// `e` / `E`, and the context menu's two extract rows.
    fn extract(&mut self, subfolder: bool, now: Instant) {
        // Inside an archive the tree is already in hand and the subject is the
        // selection; outside, the subject is the archive under the cursor and
        // the tree has to be read first.
        if self.tab().archive.is_some() {
            self.extract_selection(subfolder, now);
            return;
        }
        let Some(entry) = self.tab().cwd.dir.cursor_entry() else {
            self.toasts.notice("Nothing here to extract", now);
            return;
        };
        if !crate::archive::looks_like_archive(entry) {
            self.toasts.notice("Not an archive", now);
            return;
        }
        let path = entry.path.clone();
        self.ask_archive(
            path,
            if subfolder {
                ArchiveIntent::ExtractSubfolder
            } else {
                ArchiveIntent::ExtractHere
            },
        );
    }

    /// Extract what is selected inside the archive being browsed.
    ///
    /// The subject, in order: the rows marked with `Space`; failing that the row
    /// under the cursor; failing that the whole archive. The same ladder every
    /// other operation in the program uses for "what am I acting on", so `Enter`
    /// inside an archive needs no explanation to somebody who has used `d`.
    fn extract_selection(&mut self, subfolder: bool, now: Instant) {
        let Some(browse) = &self.tab().archive else {
            return;
        };
        let tree = Arc::clone(&browse.tree);
        let archive = browse.path.clone();
        let into = browse.real();
        let dir = &self.tab().cwd.dir;
        let mut selection: Vec<String> = dir
            .selected_paths()
            .iter()
            .filter_map(|path| browse.inner(path))
            .collect();
        if selection.is_empty() {
            if let Some(inner) = dir.cursor_entry().and_then(|e| browse.inner(&e.path)) {
                selection.push(inner);
            }
        }
        // An empty selection means the whole archive to `plan_extract`, which
        // is exactly right at the root of an empty listing.
        let Some(dest) = self.extract_dest(&archive, into, subfolder, now) else {
            return;
        };
        let borrowed: Vec<&str> = selection.iter().map(String::as_str).collect();
        self.spawn_extract(&tree, &borrowed, dest, now);
    }

    /// Where an extraction lands: the directory itself, or a fresh folder named
    /// after the archive.
    ///
    /// The subfolder is claimed through the same `name_1` ladder a paste uses,
    /// so "Extract to subfolder" twice gives `src` and `src_1` rather than
    /// merging the second one into the first.
    fn extract_dest(
        &mut self,
        archive: &Path,
        into: PathBuf,
        subfolder: bool,
        now: Instant,
    ) -> Option<PathBuf> {
        if !subfolder {
            return Some(into);
        }
        let name = crate::archive::subfolder_name(archive);
        match df_core::ops::paste::unique_name(&into, std::ffi::OsStr::new(&name), &[]) {
            Ok(dest) => match std::fs::create_dir_all(&dest) {
                Ok(()) => Some(dest),
                Err(e) => {
                    self.toasts
                        .error(df_core::DfError::io(&dest, e).to_string(), now);
                    None
                }
            },
            Err(e) => {
                self.toasts.error(e.to_string(), now);
                None
            }
        }
    }

    /// Plan and queue the extraction.
    fn spawn_extract(
        &mut self,
        tree: &df_core::archive::ArchiveTree,
        selection: &[&str],
        dest: PathBuf,
        now: Instant,
    ) {
        let plan = df_core::archive::plan_extract(tree, selection, &dest);
        if plan.is_empty() {
            // Two different nothings, and they must not read alike: an empty
            // selection, and a selection every entry of which was refused.
            let message = if plan.skipped.is_empty() {
                "Nothing to extract".to_string()
            } else {
                format!(
                    "Nothing extractable — {} refused",
                    plural(plan.skipped.len(), "entry was", "entries were")
                )
            };
            self.toasts.notice(message, now);
            return;
        }
        let job = df_core::ops::ExtractJob::new(plan);
        let slot = job.outcome();
        let id = self.engine.spawn(job);
        self.track(id, slot, vec![dest]);
    }

    /// The preview card's body: the entry under the cursor, decompressed once.
    ///
    /// One slot, keyed by the entry it holds, so a settled cursor costs nothing
    /// and a held `↓` costs one read per row it stops on rather than one per
    /// frame (PLAN §1).
    fn sync_archive_preview(&mut self) -> bool {
        let Some(browse) = &self.tab().archive else {
            let had = self.archive_preview.is_some();
            self.archive_preview = None;
            return had;
        };
        let archive = browse.path.clone();
        let Some(inner) = self
            .tab()
            .cwd
            .dir
            .cursor_entry()
            .and_then(|entry| browse.inner(&entry.path))
        else {
            let had = self.archive_preview.is_some();
            self.archive_preview = None;
            return had;
        };
        if self
            .archive_preview
            .as_ref()
            .is_some_and(|(a, i, _)| *a == archive && *i == inner)
        {
            return false;
        }
        let wanted = browse
            .tree
            .get(&inner)
            .is_some_and(crate::archive::previewable);
        let body = if wanted {
            df_core::archive::read_entry(&archive, &inner, crate::archive::PREVIEW_LIMIT)
                .ok()
                .flatten()
                .and_then(|bytes| String::from_utf8(bytes).ok())
        } else {
            None
        };
        self.archive_preview = Some((archive, inner, body));
        true
    }

    // ── Remote services (PLAN §7.6) ─────────────────────────────────────────

    /// The vfs, started the first time something asks for it.
    ///
    /// Lazy for the same reason `git` and `udisks` are: a session spent
    /// entirely in `~/Pictures` should not read `vfs.toml`, and it certainly
    /// should not hold a worker thread for a service it will never reach.
    fn vfs(&mut self) -> Arc<df_core::vfs::Vfs> {
        if self.vfs.is_none() {
            let waker = self.waker.named("vfs");
            let notifier: df_core::fs::Notifier = Arc::new(move || waker.wake());
            let vfs = df_core::vfs::Vfs::start(notifier);
            for warning in vfs.warnings() {
                log::warn!("{warning}");
            }
            self.vfs = Some(Arc::new(vfs));
        }
        // The line above filled it; the fallback keeps this infallible rather
        // than panicking in the one place a panic would take the window.
        match &self.vfs {
            Some(vfs) => Arc::clone(vfs),
            None => Arc::new(df_core::vfs::Vfs::start(Arc::new(|| {}))),
        }
    }

    /// Where the tab is, remotely, or `None` when it is on this machine.
    fn remote_at(&self) -> Option<df_core::vfs::VfsPath> {
        self.tab().remote.as_ref().map(|s| s.at.clone())
    }

    /// The directory a child process may be started in.
    ///
    /// **Not [`App::cwd`].** Three of the places the list pane can be are not
    /// directories on this machine — a remote service (`sftp://…`), the trash
    /// (`trash://`) and the inside of an archive (`…/x.zip/inner`) — and every
    /// one of them reaches `Command::current_dir` through the openers: `o` on a
    /// remote file downloads it and then *launches* on it, and a blocking
    /// opener runs a shell. A URL there is a spawn failure the user reads as
    /// "the opener is broken", and a plausible-looking path that is not a
    /// directory is worse: relative arguments would resolve somewhere else
    /// entirely. [`local_origin`](App::local_origin) already knows the real
    /// directory each of those three came from; this is the sentence that says
    /// which question is being asked.
    fn child_cwd(&self) -> PathBuf {
        let cwd = self.cwd();
        spawnable_cwd(&cwd, &self.local_origin())
    }

    /// The local directory a jump away from here should be able to come back
    /// to: the one on screen, or the one the virtual listing already remembers.
    fn local_origin(&self) -> PathBuf {
        if let Some(session) = &self.tab().remote {
            return session.origin.clone();
        }
        if let Some(view) = &self.tab().trash {
            return view.origin.clone();
        }
        if let Some(browse) = &self.tab().archive {
            return browse.real();
        }
        self.cwd()
    }

    /// Go to a remote place — `g 1`, a breadcrumb click, `←`, `→`, or a
    /// palette row.
    ///
    // VERIFY-LIVE: `g 1` against showandtour1 with the key in the agent. The
    // sticky "Connecting to showandtour1…" should appear, the pane should show
    // "loading…" after 150 ms rather than an empty directory, and both should
    // be replaced by rows in one step — no flash of "empty" in between. A
    // machine that is *down* should end in a readable toast within
    // `df_core::vfs::CONNECT_TIMEOUT` and never leave the sticky up.
    fn enter_remote(&mut self, at: df_core::vfs::VfsPath, now: Instant) {
        let vfs = self.vfs();
        if vfs.service(&at.service).is_none() {
            // Naming what *is* configured is the difference between an error
            // and an error a person can act on: the usual cause is a typo in a
            // bookmark, and the fix is in the sentence.
            let known: Vec<&str> = vfs.services().iter().map(|s| s.name.as_str()).collect();
            let known = if known.is_empty() {
                "vfs.toml defines no services".to_string()
            } else {
                format!("vfs.toml has {}", known.join(", "))
            };
            self.toasts
                .error(format!("No service called {} — {known}", at.service), now);
            return;
        }

        let origin = self.local_origin();
        let session = match self.tabs.active_mut().remote.take() {
            // Staying on the same service keeps the cache and the origin: this
            // is a step, not a new session.
            Some(mut session) if session.at.service == at.service => {
                session.at = at.clone();
                session
            }
            _ => crate::remote::Session::new(at.clone(), origin),
        };
        let (mgr, sort) = (self.mgr.clone(), self.sort());
        let wanted = self.tabs.active_mut().show_remote(session, &mgr, sort, now);
        // Leaving is leaving: a visual run is anchored to a row in a listing
        // that is no longer on screen.
        self.visual = None;
        self.focus = Focus::List;
        self.close_player();
        self.preview.cancel();
        self.rewatch();
        self.remote_preview = None;
        self.remote_hover = None;
        if let Some(at) = wanted {
            self.scan_remote(at, now);
        }
    }

    /// Queue a listing and remember its token.
    ///
    // VERIFY-LIVE: hold `↓` through a large remote directory and then `→` into
    // one, twice quickly. Only the last directory's rows may land; the tokens
    // are what drop the rest, and a stale batch appearing in the wrong pane is
    // the failure this is guarding against.
    fn scan_remote(&mut self, at: df_core::vfs::VfsPath, now: Instant) {
        let vfs = self.vfs();
        // The one wait the user can see, said out loud — and as a *sticky*
        // toast, which has no clock: a connect that takes twelve seconds must
        // not have its own notice expire underneath it. It comes down on the
        // service's first answer, whichever answer that is.
        if !self.remote_connected.contains(&at.service) {
            self.toasts
                .sticky(format!("Connecting to {}…", at.service), now);
        }
        let token = vfs.scan(at.clone());
        if let Some(session) = &mut self.tabs.active_mut().remote {
            // A listing this tab has walked away from is not wanted; dropping
            // the old token is also what stops its batches landing in the new
            // directory's pane.
            if let Some((old, _)) = session.pending.replace((token, at)) {
                if old != token {
                    vfs.cancel(old);
                }
            }
        }
    }

    /// Whatever the vfs has said since the last frame.
    fn poll_vfs(&mut self, now: Instant) -> bool {
        let Some(vfs) = self.vfs.clone() else {
            return false;
        };
        let updates = vfs.drain();
        if updates.is_empty() {
            return false;
        }
        for update in updates {
            let service = update.dir().service.clone();
            let mut wanted = false;
            for tab in self.tabs.iter_mut() {
                wanted |= apply_vfs(tab, &update);
            }
            match update {
                df_core::vfs::VfsUpdate::Started { .. } => {
                    if self.remote_connected.insert(service) {
                        self.toasts.clear_sticky(now);
                    }
                }
                df_core::vfs::VfsUpdate::Failed { error, .. } => {
                    // The connection is up as far as the toast is concerned:
                    // whatever happens next, the "connecting…" notice has been
                    // answered and must come down (PLAN §7.6's "never hangs").
                    self.remote_connected.insert(service);
                    self.toasts.clear_sticky(now);
                    if wanted {
                        self.toasts.error(error.to_string(), now);
                    }
                }
                _ => {}
            }
        }
        true
    }

    /// Spawn a remote operation on the pool.
    ///
    /// Every remote verb goes through here, including the ones that are a
    /// single packet: an `ssh` round trip between two frames is a frozen
    /// window, and "this one is quick" is how that gets written by accident.
    fn spawn_remote<F>(&mut self, name: String, lane: Lane, work: F)
    where
        F: FnOnce(&df_core::vfs::Vfs, &TaskCtx) -> std::result::Result<RemoteDone, String>
            + Send
            + 'static,
    {
        let vfs = self.vfs();
        let slot: Arc<std::sync::Mutex<Option<std::result::Result<RemoteDone, String>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let job_slot = Arc::clone(&slot);
        let mut work = Some(work);
        let job = FnJob::new(name, lane, move |ctx| {
            // `FnJob` is `FnMut` because the engine may retry; a remote verb is
            // taken once and a retry finds nothing to do rather than sending a
            // second `RENAME` the user did not ask for.
            let Some(work) = work.take() else {
                return Ok(());
            };
            let result = work(&vfs, ctx);
            match job_slot.lock() {
                Ok(mut guard) => *guard = Some(result),
                Err(poisoned) => *poisoned.into_inner() = Some(result),
            }
            Ok(())
        });
        let id = self.engine.spawn(job);
        self.remote_ops.push(PendingRemote { id, slot });
    }

    /// Have any remote operations landed?
    fn poll_remote_ops(&mut self, now: Instant) -> bool {
        if self.remote_ops.is_empty() {
            return false;
        }
        let mut landed: Vec<std::result::Result<RemoteDone, String>> = Vec::new();
        self.remote_ops.retain(|op| {
            let result = match op.slot.lock() {
                Ok(mut guard) => guard.take(),
                Err(poisoned) => poisoned.into_inner().take(),
            };
            match result {
                Some(result) => {
                    landed.push(result);
                    false
                }
                // Still running. A job that ends without filling its slot —
                // cancelled before a worker picked it up — is dropped by
                // [`App::task_event`], so this cannot leak.
                None => true,
            }
        });
        if landed.is_empty() {
            return false;
        }
        for result in landed {
            match result {
                Err(message) => {
                    // A cancelled operation is not a failure and stays quiet,
                    // the same rule `finish_op` follows.
                    if message != df_core::DfError::Cancelled.to_string() {
                        self.toasts.error(message, now);
                    }
                    if let Some(preview) = &mut self.remote_preview {
                        preview.loading = false;
                    }
                }
                Ok(done) => self.apply_remote_done(done, now),
            }
        }
        true
    }

    fn apply_remote_done(&mut self, done: RemoteDone, now: Instant) {
        if let Some(at) = &done.invalidate {
            if let Some(session) = &mut self.tabs.active_mut().remote {
                session.invalidate(at);
            }
            // Only re-list if it is the directory on screen; invalidating is
            // enough for anywhere else, and a listing nobody is looking at is a
            // round trip nobody asked for.
            if self.remote_at().as_ref() == Some(at) {
                let (mgr, sort) = (self.mgr.clone(), self.sort());
                if let Some(at) = self.tabs.active_mut().refresh_remote(&mgr, sort, now) {
                    self.scan_remote(at, now);
                }
            }
        }
        for dir in &done.dirs {
            self.rescan(dir, now);
        }
        if let Some((url, local)) = done.preview {
            let body = read_preview_text(&local);
            self.temps.remember(url.clone(), local);
            if let Some(preview) = &mut self.remote_preview {
                if preview.url == url {
                    preview.body = body;
                    preview.loading = false;
                }
            }
        }
        if let Some((url, local)) = done.open {
            self.temps.remember(url, local.clone());
            self.open_local_temp(local, now);
        }
        if let Some(probe) = done.upload {
            self.plan_upload(probe, now);
        }
        if !done.message.is_empty() {
            self.toasts.confirm(done.message, now);
        }
    }

    /// `o` / `Enter` on a remote file: the download, then the local opener.
    ///
    /// The temp file is kept and reused, so opening the same row twice costs
    /// one download — and it is *accounted for*, so quitting removes it
    /// (see [`crate::remote::Temps`]).
    ///
    // VERIFY-LIVE: `o` on a remote image. It should download once (visible in
    // the `w` panel with progress), open in the configured viewer, and open
    // *instantly* the second time. On quit, `$TMPDIR/delightfile-vfs-<pid>/`
    // must be gone.
    fn open_remote(&mut self, at: df_core::vfs::VfsPath, name: String, now: Instant) {
        let url = at.to_url();
        if let Some(local) = self.temps.get(&url).map(Path::to_path_buf) {
            self.open_local_temp(local, now);
            return;
        }
        self.toasts.notice(format!("Downloading {name}…"), now);
        self.spawn_remote(format!("Download {name}"), Lane::Macro, move |vfs, ctx| {
            let local = vfs.download_to_temp(&at, ctx).map_err(|e| e.to_string())?;
            Ok(RemoteDone {
                open: Some((url, local)),
                ..RemoteDone::default()
            })
        });
    }

    /// Hand a downloaded file to the opener rules, as though it were local —
    /// which, by this point, it is.
    fn open_local_temp(&mut self, local: PathBuf, now: Instant) {
        let entry = match df_core::fs::Entry::read(&local) {
            Ok(entry) => entry,
            Err(e) => {
                self.toasts.error(e.to_string(), now);
                return;
            }
        };
        let Some(choice) = open::choices_for(&self.config, &entry).into_iter().next() else {
            self.toasts
                .notice(format!("No opener rule matches {}", entry.name), now);
            return;
        };
        self.launch(&choice, vec![local], now);
    }

    /// The preview card's body: the row under the cursor, downloaded once it
    /// has been rested on.
    ///
    /// Two gates before a byte moves — [`crate::remote::previewable`] and the
    /// debounce — because this is the one place in the program where moving the
    /// cursor spends somebody's bandwidth.
    ///
    // VERIFY-LIVE: hold `↓` through a remote directory of source files. The
    // `w` panel must stay empty — not one download per row — and a pause of a
    // beat on one row must produce exactly one "Preview …" task.
    fn sync_remote_preview(&mut self, now: Instant) -> bool {
        if self.tab().remote.is_none() {
            let had = self.remote_preview.is_some() || self.remote_hover.is_some();
            self.remote_preview = None;
            self.remote_hover = None;
            return had;
        }
        let Some(entry) = self.tab().cwd.dir.cursor_entry().cloned() else {
            let had = self.remote_preview.is_some();
            self.remote_preview = None;
            self.remote_hover = None;
            return had;
        };
        let url = entry.path.to_string_lossy().into_owned();
        if self.remote_preview.as_ref().is_some_and(|p| p.url == url) {
            return false;
        }

        // A row with no body to fetch settles immediately: the card is the
        // facts and the reason there is nothing under them.
        if !crate::remote::previewable(&entry) {
            self.remote_hover = None;
            self.remote_preview = Some(RemotePreview {
                url,
                body: None,
                loading: false,
            });
            return true;
        }
        // Already downloaded — `o` on this row earlier in the session, or a
        // second visit. No round trip, and the body is there this frame.
        if let Some(local) = self.temps.get(&url).map(Path::to_path_buf) {
            self.remote_hover = None;
            self.remote_preview = Some(RemotePreview {
                body: read_preview_text(&local),
                url,
                loading: false,
            });
            return true;
        }

        match &self.remote_hover {
            Some((resting, since)) if *resting == url => {
                if now.duration_since(*since) < crate::remote::PREVIEW_DEBOUNCE {
                    // Not yet. The deadline is scheduled by
                    // `remote_preview_deadline`, so this costs no frames.
                    return false;
                }
            }
            _ => {
                self.remote_hover = Some((url, now));
                return true;
            }
        }

        self.remote_hover = None;
        self.remote_preview = Some(RemotePreview {
            url: url.clone(),
            body: None,
            loading: true,
        });
        let at = df_core::vfs::VfsPath::parse(&url);
        let Some(at) = at else { return true };
        // `Lane::Micro`: it is a small read the user is waiting on, and putting
        // it behind a queued 4 GB download would make the pane lie for minutes.
        self.spawn_remote(
            format!("Preview {}", entry.name),
            Lane::Micro,
            move |vfs, ctx| {
                let local = vfs.download_to_temp(&at, ctx).map_err(|e| e.to_string())?;
                Ok(RemoteDone {
                    preview: Some((url, local)),
                    ..RemoteDone::default()
                })
            },
        );
        true
    }

    /// When the resting cursor is due its preview download, if one is pending.
    fn remote_preview_deadline(&self, now: Instant) -> Option<Duration> {
        let (_, since) = self.remote_hover.as_ref()?;
        let due = (*since + crate::remote::PREVIEW_DEBOUNCE).saturating_duration_since(now);
        (!due.is_zero()).then_some(due)
    }

    /// `a` with a trailing `/` on a remote service. Files are not offered:
    /// SFTP can make an empty one, but "create a file here" on a machine you
    /// are not going to edit on is a gesture with no follow-through, and
    /// uploading is the way files get there.
    fn remote_create(&mut self, at: df_core::vfs::VfsPath, text: &str) -> Result<(), String> {
        let name = text.trim().trim_end_matches('/');
        if name.is_empty() {
            return Err("no name given".to_string());
        }
        if !text.trim().ends_with('/') {
            return Err("only folders can be created remotely — end the name with /".to_string());
        }
        if name.contains('/') {
            return Err("one folder at a time: remote create makes no parents".to_string());
        }
        let target = at.join(name);
        let label = name.to_string();
        self.spawn_remote(format!("Make {label}"), Lane::Micro, move |vfs, ctx| {
            vfs.mkdir(&target, ctx).map_err(|e| e.to_string())?;
            Ok(RemoteDone {
                message: format!("Created folder {label}"),
                invalidate: Some(at),
                ..RemoteDone::default()
            })
        });
        Ok(())
    }

    /// `r` on a remote row.
    fn remote_rename(&mut self, at: df_core::vfs::VfsPath, text: &str) -> Result<(), String> {
        let name = text.trim();
        if name.is_empty() {
            return Err("no name given".to_string());
        }
        if name.contains('/') {
            return Err("a rename is a name, not a path".to_string());
        }
        let Some(from) = self
            .tab()
            .cwd
            .dir
            .cursor_entry()
            .and_then(|entry| crate::remote::at_of(&entry.path))
        else {
            return Err("Nothing under the cursor".to_string());
        };
        let to = at.join(name);
        let old_url = from.to_url();
        let label = name.to_string();
        // The temp file was a copy of the *old* name, and an opener that
        // sniffs extensions must not be handed it under the new one.
        self.temps.forget(&old_url);
        self.spawn_remote(
            format!("Rename to {label}"),
            Lane::Micro,
            move |vfs, ctx| {
                vfs.rename(&from, &to, ctx).map_err(|e| e.to_string())?;
                Ok(RemoteDone {
                    message: format!("Renamed to {label}"),
                    invalidate: Some(at),
                    ..RemoteDone::default()
                })
            },
        );
        Ok(())
    }

    /// `d` on a remote selection, once the confirm has been answered.
    ///
    /// No trash, and the dialog said so. Directories go through `RMDIR` and
    /// files through `REMOVE`; a non-empty directory fails with the server's
    /// own words, which is the honest outcome — a recursive remote delete is a
    /// walk plus N round trips, and doing it silently behind one keystroke is
    /// how people lose a `node_modules` they meant to keep.
    ///
    // VERIFY-LIVE: `d` on a remote file and on a non-empty remote folder. The
    // dialog body must read "There is no trash on the server", the file must
    // go, and the folder must fail with the server's own words rather than
    // being emptied.
    fn remote_delete(&mut self, paths: Vec<PathBuf>, now: Instant) {
        let Some(at) = self.remote_at() else { return };
        let targets: Vec<(df_core::vfs::VfsPath, bool)> = paths
            .iter()
            .filter_map(|path| {
                let entry = self
                    .tab()
                    .cwd
                    .dir
                    .entries()
                    .iter()
                    .find(|entry| &entry.path == path)?;
                Some((crate::remote::at_of(path)?, entry.is_dir()))
            })
            .collect();
        if targets.is_empty() {
            self.toasts.notice("Nothing selected", now);
            return;
        }
        for (place, _) in &targets {
            self.temps.forget(&place.to_url());
        }
        let count = targets.len();
        let here = at.clone();
        self.spawn_remote(
            format!("Delete {}", plural(count, "remote item", "remote items")),
            Lane::Macro,
            move |vfs, ctx| {
                let mut gone = 0;
                let mut failure: Option<String> = None;
                for (place, is_dir) in &targets {
                    let result = if *is_dir {
                        vfs.rmdir(place, ctx)
                    } else {
                        vfs.remove(place, ctx)
                    };
                    match result {
                        Ok(()) => gone += 1,
                        // The first failure is the one reported, and the rest
                        // of the selection still goes: a permission problem on
                        // one file is not a reason to keep the other thirty.
                        Err(e) => {
                            failure.get_or_insert_with(|| e.to_string());
                        }
                    }
                }
                match failure {
                    // Something went, and something did not: the count is the
                    // fact, and the error is the sentence.
                    Some(message) if gone > 0 => Err(format!(
                        "Deleted {} — {message}",
                        plural(gone, "item", "items")
                    )),
                    Some(message) => Err(message),
                    None => Ok(RemoteDone {
                        message: format!("Deleted {}", plural(gone, "remote item", "remote items")),
                        invalidate: Some(here),
                        ..RemoteDone::default()
                    }),
                }
            },
        );
    }

    /// `p` in a local directory with remote paths yanked.
    ///
    // VERIFY-LIVE: `y` on a remote selection, `←` out to a local folder, `p`.
    // The `w` panel should show byte progress per file, the local pane should
    // fill in as each lands (inotify), and a name that is already taken should
    // become `name_1` rather than overwriting.
    fn remote_download(&mut self, sources: Vec<PathBuf>, dest: PathBuf, now: Instant) {
        let places: Vec<df_core::vfs::VfsPath> = sources
            .iter()
            .filter_map(|p| crate::remote::at_of(p))
            .collect();
        if places.is_empty() {
            self.toasts.notice("Nothing remote to download", now);
            return;
        }
        let count = places.len();
        let into = dest.clone();
        self.spawn_remote(
            format!(
                "Download {} → {}",
                plural(count, "file", "files"),
                dest.display()
            ),
            Lane::Macro,
            move |vfs, ctx| {
                let mut done = 0;
                let mut failure: Option<String> = None;
                for place in &places {
                    // The name is claimed through the same `name_1` ladder a
                    // paste uses, so a download never silently overwrites what
                    // is already in the directory (PLAN §5).
                    let local = match df_core::ops::paste::unique_name(
                        &into,
                        std::ffi::OsStr::new(place.name()),
                        &[],
                    ) {
                        Ok(path) => path,
                        Err(e) => {
                            failure.get_or_insert_with(|| e.to_string());
                            continue;
                        }
                    };
                    match vfs.download(place, &local, ctx) {
                        Ok(_) => done += 1,
                        Err(e) => {
                            failure.get_or_insert_with(|| e.to_string());
                        }
                    }
                }
                match failure {
                    Some(message) if done > 0 => Err(format!(
                        "Downloaded {} — {message}",
                        plural(done, "file", "files")
                    )),
                    Some(message) => Err(message),
                    None => Ok(RemoteDone {
                        message: format!("Downloaded {}", plural(done, "file", "files")),
                        dirs: vec![into],
                        ..RemoteDone::default()
                    }),
                }
            },
        );
    }

    /// `p` inside a remote directory with local paths yanked — and the same
    /// runner a drop from a local pane uses (PLAN §7.1's target seam hands both
    /// a `Vec<PathBuf>` and a destination, so there is one upload here rather
    /// than one per gesture).
    ///
    // VERIFY-LIVE: both doors. `y` locally then `p` in a remote folder, and a
    // drag from the parent column onto the remote list pane — the second must
    // say "Uploaded as a copy" and leave the local originals alone. The remote
    // listing must refresh itself when the upload lands, without an `R`.
    fn remote_upload(&mut self, sources: Vec<PathBuf>, dest: df_core::vfs::VfsPath, now: Instant) {
        let files: Vec<PathBuf> = sources.iter().filter(|p| p.is_file()).cloned().collect();
        let folders = sources.len() - files.len();
        if files.is_empty() {
            // Said rather than silently doing nothing: a directory in the
            // clipboard is the common case and the reason is not obvious.
            self.toasts.notice(
                if folders > 0 {
                    "Only files upload — folders are not walked"
                } else {
                    "Nothing local to upload"
                },
                now,
            );
            return;
        }
        if folders > 0 {
            self.toasts.notice(
                format!(
                    "Skipping {}: only files upload",
                    plural(folders, "folder", "folders")
                ),
                now,
            );
        }
        // **The destination is asked about before a byte moves.** A download
        // has always claimed a free local name (see `remote_download`); this is
        // the same promise pointing the other way, and it is a round trip, so
        // it goes on the pool and comes back through `apply_remote_done` like
        // every other remote verb.
        let here = dest.clone();
        let names: Vec<String> = files
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        self.spawn_remote(
            format!("Check {}", dest.to_url()),
            Lane::Micro,
            move |vfs, ctx| {
                let mut taken = Vec::new();
                for name in &names {
                    let at = here.join(name);
                    match vfs.stat(&at, false, ctx) {
                        Ok(attrs) => taken.push(df_core::vfs::stat_entry(&at, attrs)),
                        // Nothing there is the answer this is hoping for.
                        Err(df_core::vfs::VfsError::Status { status, .. })
                            if status.code == df_core::vfs::StatusCode::NoSuchFile => {}
                        // Anything else — a permission problem on the
                        // directory, a dropped link — must not be read as "the
                        // name is free". The upload does not start.
                        Err(e) => return Err(e.to_string()),
                    }
                }
                Ok(RemoteDone {
                    upload: Some(UploadProbe {
                        dest: here,
                        sources: files,
                        taken,
                    }),
                    ..RemoteDone::default()
                })
            },
        );
    }

    /// The probe came back: plan the upload, and ask about anything in the way.
    ///
    /// The plan is a [`PastePlan`](df_core::ops::paste::PastePlan) with
    /// `sftp://…` paths in it, so the collision goes to the **same** dialog and
    /// the same state machine a local paste uses — Overwrite / Skip / Rename /
    /// apply-to-all, `Esc` cancels the whole thing — rather than to a second
    /// resolver that would have to be kept in step with the first.
    fn plan_upload(&mut self, probe: UploadProbe, now: Instant) {
        let plan = crate::remote::plan_upload(&probe.sources, &probe.dest, &probe.taken);
        if plan.is_settled() {
            self.spawn_paste(plan, now);
            return;
        }
        // The destinations' facts came off the wire; `Facts::read` would call
        // them missing, which is the one thing a card about overwriting must
        // not say.
        let facts: HashMap<PathBuf, dialog::Facts> = probe
            .taken
            .iter()
            .map(|entry| (entry.path.clone(), dialog::Facts::of(entry.clone())))
            .collect();
        self.dialog = Some(Dialog::Conflict(Box::new(ConflictDialog::with_facts(
            plan, facts,
        ))));
        self.sync_context();
    }

    /// Carry out a settled upload plan.
    ///
    /// Two verbs, and which one an item gets is the answer the user gave:
    /// `overwrite` is [`Vfs::upload`](df_core::vfs::Vfs::upload), which is the
    /// atomic scratch-file-then-rename, and everything else is
    /// [`Vfs::upload_new`](df_core::vfs::Vfs::upload_new), which climbs the
    /// `name_1` ladder **on the server, immediately before the bytes move** —
    /// the plan's suggestion came from a listing, and a listing is a
    /// photograph.
    ///
    // VERIFY-LIVE: `y` a local file whose name is already on the server, `p` in
    // the remote folder. The dialog must show the remote file's real size and
    // date on the right; `o` must replace it, `r` must land `name_1`, `s` must
    // leave both alone, and `Esc` must cancel without a byte moving.
    fn spawn_upload(&mut self, plan: df_core::ops::paste::PastePlan, now: Instant) {
        let Some(dest) = crate::remote::at_of(&plan.dest_dir) else {
            return;
        };
        let items: Vec<(PathBuf, df_core::vfs::VfsPath, bool)> = plan
            .ready
            .iter()
            .filter_map(|item| {
                Some((
                    item.src.clone(),
                    crate::remote::at_of(&item.dst)?,
                    item.overwrite,
                ))
            })
            .collect();
        if items.is_empty() {
            self.toasts.notice("Nothing left to upload", now);
            return;
        }
        let count = items.len();
        let here = dest.clone();
        self.spawn_remote(
            format!(
                "Upload {} → {}",
                plural(count, "file", "files"),
                dest.to_url()
            ),
            Lane::Macro,
            move |vfs, ctx| {
                let mut done = 0;
                let mut failure: Option<String> = None;
                for (local, remote, overwrite) in &items {
                    let result = if *overwrite {
                        vfs.upload(local, remote, ctx).map(|n| (n, remote.clone()))
                    } else {
                        vfs.upload_new(local, remote, ctx)
                    };
                    match result {
                        Ok(_) => done += 1,
                        Err(e) => {
                            failure.get_or_insert_with(|| e.to_string());
                        }
                    }
                }
                match failure {
                    Some(message) if done > 0 => Err(format!(
                        "Uploaded {} — {message}",
                        plural(done, "file", "files")
                    )),
                    Some(message) => Err(message),
                    None => Ok(RemoteDone {
                        message: format!("Uploaded {}", plural(done, "file", "files")),
                        invalidate: Some(here),
                        ..RemoteDone::default()
                    }),
                }
            },
        );
    }

    // ── The trash, browsed (PLAN §7.4) ──────────────────────────────────────

    /// `g t`, and the palette's "Open trash".
    fn open_trash(&mut self, now: Instant) {
        let origin = self.local_origin();
        self.show_trash(origin, now);
    }

    /// Read the trash and put it in the list pane.
    ///
    /// Synchronous, unlike every other listing in this program, and defensibly
    /// so: a trash listing is one `read_dir` of `info/` plus a short file read
    /// per item, over a directory whose size is bounded by what one person has
    /// deleted and not emptied. The asynchronous machinery exists for
    /// directories that can be enormous; this is not one.
    fn show_trash(&mut self, origin: PathBuf, now: Instant) {
        let trash = match df_core::ops::Trash::home() {
            Ok(trash) => trash,
            Err(e) => {
                self.toasts.error(e.to_string(), now);
                return;
            }
        };
        let items = match trash.list() {
            Ok(items) => items,
            Err(e) => {
                self.toasts.error(e.to_string(), now);
                return;
            }
        };
        self.trash_notes = crate::trashview::notes(&items);
        let view = crate::trashview::View { items, origin };
        let (mgr, sort) = (self.mgr.clone(), self.sort());
        self.tabs.active_mut().show_trash(view, &mgr, sort, now);
        self.visual = None;
        self.focus = Focus::List;
        self.close_player();
        self.rewatch();
    }

    /// Rebuild the trash listing from disk, keeping the cursor.
    fn refresh_trash(&mut self, now: Instant) {
        let Some(origin) = self.tab().trash.as_ref().map(|v| v.origin.clone()) else {
            return;
        };
        self.show_trash(origin, now);
    }

    /// The trashed items a set of row paths names.
    fn trash_targets(&self) -> Vec<df_core::ops::TrashedItem> {
        let paths = self.targets();
        self.tab()
            .trash
            .as_ref()
            .map(|view| view.items_for(&paths))
            .unwrap_or_default()
    }

    /// `Enter` / `r` in the trash: put things back where they came from.
    ///
    /// Every refusal is checked *before* anything moves and reported as one
    /// sentence, because a restore of forty items that stops on the third is a
    /// worse outcome than one that says which three cannot go back. The move
    /// itself is [`df_core::ops::trash::restore`] — the same function `u` runs
    /// after a `d`, so the two doors to a restore cannot drift apart.
    fn trash_restore(&mut self, now: Instant) {
        let items = self.trash_targets();
        if items.is_empty() {
            self.toasts.notice("Nothing selected", now);
            return;
        }
        let ctx = TaskCtx::detached();
        let mut restored = 0;
        let mut refusal: Option<String> = None;
        for item in &items {
            if let Some(message) = crate::trashview::restore_refusal(item) {
                refusal.get_or_insert(message);
                continue;
            }
            match df_core::ops::trash::restore(item, &ctx) {
                Ok(_) => restored += 1,
                Err(e) => {
                    refusal.get_or_insert_with(|| e.to_string());
                }
            }
        }
        self.refresh_trash(now);
        match refusal {
            Some(message) if restored > 0 => self.toasts.error(
                format!("Restored {} — {message}", plural(restored, "item", "items")),
                now,
            ),
            Some(message) => self.toasts.error(message, now),
            None => self.toasts.confirm(
                format!("Restored {}", plural(restored, "item", "items")),
                now,
            ),
        }
    }

    /// `D` in the trash, and "Empty trash" — the same job with a different
    /// subject. Never journalled: there is nothing to record.
    fn trash_purge(&mut self, items: Vec<df_core::ops::TrashedItem>, now: Instant) {
        if items.is_empty() {
            self.toasts.notice("Nothing to destroy", now);
            return;
        }
        let count = items.len();
        let slot: Arc<std::sync::Mutex<Option<std::result::Result<RemoteDone, String>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let job_slot = Arc::clone(&slot);
        let job = FnJob::new(
            format!("Destroy {}", plural(count, "trashed item", "trashed items")),
            Lane::Macro,
            move |ctx| {
                let mut gone = 0;
                let mut failure: Option<String> = None;
                for item in &items {
                    match df_core::ops::purge(item, ctx) {
                        Ok(()) => gone += 1,
                        Err(e) => {
                            failure.get_or_insert_with(|| e.to_string());
                        }
                    }
                }
                let result = match failure {
                    Some(message) if gone > 0 => Err(format!(
                        "Destroyed {} — {message}",
                        plural(gone, "item", "items")
                    )),
                    Some(message) => Err(message),
                    None => Ok(RemoteDone {
                        message: format!("Destroyed {}", plural(gone, "item", "items")),
                        ..RemoteDone::default()
                    }),
                };
                match job_slot.lock() {
                    Ok(mut guard) => *guard = Some(result),
                    Err(poisoned) => *poisoned.into_inner() = Some(result),
                }
                Ok(())
            },
        );
        let id = self.engine.spawn(job);
        // The purge shares the remote pipeline's bookkeeping, which is not a
        // pun: both are "a job whose result is a message and a listing that has
        // to be re-read", and a second copy of that would be a second place for
        // the toast to go missing.
        self.remote_ops.push(PendingRemote { id, slot });
        // The rows are gone the moment the job is queued as far as the *view*
        // is concerned; the re-read below is what makes that true on screen.
        self.refresh_trash(now);
    }

    /// `y` / `x`.
    fn set_clipboard(&mut self, cut: bool, now: Instant) {
        let paths = self.targets();
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        self.clipboard = if cut {
            Clipboard::cut(paths)
        } else {
            Clipboard::yank(paths)
        };
        let verb = if cut { "Cut" } else { "Yanked" };
        self.toasts
            .notice(format!("{verb} {}", plural(n, "item", "items")), now);
    }

    /// `X`: the yank is off. The marks come off the rows with it.
    ///
    /// With nothing yanked, the same key empties the **basket** — it is the
    /// other thing the program is carrying, `X` already means "stop carrying
    /// this", and a tray of thirty files with no way to put them all down but
    /// thirty clicks on an `×` would be a collection you cannot get rid of. The
    /// order matches `p`'s ladder (see [`crate::basket`]): the clipboard first,
    /// because it is the more recent gesture.
    fn unyank(&mut self, now: Instant) {
        if !self.clipboard.is_empty() {
            self.clipboard.clear();
            self.toasts.notice("Clipboard cleared", now);
            return;
        }
        if !self.basket.is_empty() {
            let n = self.basket.len();
            self.basket.clear();
            self.clamp_basket();
            self.toasts.notice(
                format!("Basket emptied — {}", plural(n, "file", "files")),
                now,
            );
        }
    }

    /// `p` / `P`. Conflicts open the dialog; a settled plan goes straight to
    /// the pool.
    fn paste(&mut self, force: bool, now: Instant) {
        // The three-way ladder, spelled out in [`crate::basket`]: the internal
        // clipboard, then the basket, then whatever another application put on
        // the system clipboard. The rule itself is a pure function there so
        // that it is a test rather than three `if`s.
        match crate::basket::Precedence::of(!self.clipboard.is_empty(), !self.basket.is_empty()) {
            crate::basket::Precedence::Clipboard => {
                let clipboard = self.clipboard.clone();
                self.paste_from(&clipboard, force, now);
            }
            crate::basket::Precedence::Basket => {
                // A copy, always. A basket gathered over five directories has
                // no single origin to have been *cut* from, and a `p` that
                // emptied five folders at once would be the most destructive
                // keystroke in the program.
                let clipboard = Clipboard::yank(self.basket.paths().to_vec());
                self.paste_from(&clipboard, force, now);
            }
            crate::basket::Precedence::System => self.paste_system(force, now),
        }
    }

    /// Plan and run a paste of `clipboard` into the current directory.
    fn paste_from(&mut self, clipboard: &Clipboard, force: bool, now: Instant) {
        self.paste_into(clipboard, self.cwd(), force, now);
    }

    /// The same, into a directory that is not necessarily the one you are in.
    ///
    /// The destination is a parameter because a *drop* names one (PLAN §7.1):
    /// a folder row, a breadcrumb, another tab. Everything downstream — the
    /// conflict dialog, the task, the journal record, the undo toast — is the
    /// same code either way, which is the point of routing a drop through here
    /// rather than giving it a pipeline of its own.
    fn paste_into(&mut self, clipboard: &Clipboard, dest: PathBuf, force: bool, now: Instant) {
        // PLAN §7.6: one clipboard, one `p`, and the meaning read off the two
        // ends rather than off a mode (see [`crate::remote::Transfer`]).
        match crate::remote::Transfer::of(&clipboard.paths, &dest) {
            crate::remote::Transfer::Local => {}
            crate::remote::Transfer::Download => {
                self.remote_download(clipboard.paths.clone(), dest, now);
                return;
            }
            crate::remote::Transfer::Upload => {
                let Some(at) = crate::remote::at_of(&dest) else {
                    return;
                };
                self.remote_upload(clipboard.paths.clone(), at, now);
                return;
            }
            crate::remote::Transfer::Across => {
                // Refused *by name*: it would be a download and an upload
                // through this machine, and the user should be told that is
                // what they are asking for rather than working it out from a
                // progress bar that runs twice.
                self.toasts.notice(
                    "Server to server would come through this machine — download it first",
                    now,
                );
                return;
            }
            crate::remote::Transfer::Mixed => {
                self.toasts.notice(
                    "Local and remote files in one paste — yank one or the other",
                    now,
                );
                return;
            }
        }
        let plan = match plan_paste(clipboard, &dest, force) {
            Ok(plan) => plan,
            Err(e) => {
                self.toasts.error(e.to_string(), now);
                return;
            }
        };
        if !plan.is_settled() {
            self.dialog = Some(Dialog::Conflict(Box::new(ConflictDialog::new(plan))));
            self.sync_context();
            return;
        }
        self.spawn_paste(plan, now);
    }

    fn spawn_paste(&mut self, plan: df_core::ops::paste::PastePlan, now: Instant) {
        // **The one place a settled plan is carried out**, and therefore the
        // one place that has to know a plan can be pointed at a server. Both
        // doors into it — a paste with nothing in the way, and the conflict
        // dialog's `Settled` — arrive here, so an upload cannot reach the local
        // paste engine (which would `copy` an `sftp://…` string into a
        // directory named after a URL) by anybody forgetting a branch.
        if crate::remote::at_of(&plan.dest_dir).is_some() {
            self.spawn_upload(plan, now);
            return;
        }
        if plan.ready.is_empty() {
            self.toasts.notice("Nothing left to paste", now);
            return;
        }
        let dirs = Self::affected(
            &plan.ready.iter().map(|i| i.src.clone()).collect::<Vec<_>>(),
            Some(&plan.dest_dir),
        );
        let cut = plan.mode == PasteMode::Cut;
        let job = PasteJob::new(plan);
        let slot = job.outcome();
        let id = self.engine.spawn(job);
        self.track(id, slot, dirs);
        // A cut is spent by its paste: pasting it a second time would move
        // files that are no longer where the clipboard says they are.
        if cut {
            self.clipboard.clear();
        }
    }

    /// `d` and `D`, both of which ask first.
    fn open_confirm(&mut self, kind: ConfirmKind, now: Instant) {
        // "Empty trash" is about the whole trash, not about what is selected —
        // and it is the one confirm whose body has to be able to say how many
        // things it is destroying even when nothing is highlighted.
        let paths = if kind == ConfirmKind::EmptyTrash {
            match &self.tab().trash {
                Some(view) => view.items.iter().map(|i| i.files_path()).collect(),
                None => Vec::new(),
            }
        } else {
            self.targets()
        };
        if paths.is_empty() {
            self.toasts.notice(
                if kind == ConfirmKind::EmptyTrash {
                    "The trash is already empty"
                } else {
                    "Nothing selected"
                },
                now,
            );
            return;
        }
        self.dialog = Some(Dialog::Confirm(Confirm::new(kind, paths)));
        self.sync_context();
    }

    /// The confirm was answered yes.
    fn run_confirm(&mut self, confirm: Confirm, now: Instant) {
        let dirs = Self::affected(&confirm.paths, None);
        let (id, slot) = match confirm.kind {
            ConfirmKind::Trash => {
                let job = TrashJob::new(confirm.paths);
                let slot = job.outcome();
                (self.engine.spawn(job), slot)
            }
            ConfirmKind::Delete => {
                let job = DeleteJob::new(confirm.paths);
                let slot = job.outcome();
                (self.engine.spawn(job), slot)
            }
            // The three that do not go through the ops pipeline: two of them
            // have no local inverse to journal, and the third is not on this
            // machine at all.
            ConfirmKind::RemoteDelete => {
                self.remote_delete(confirm.paths, now);
                return;
            }
            // Both resolve the items from `confirm.paths` — the list the dialog
            // rendered and the user assented to — rather than re-deriving them
            // from the selection now. A watcher event while the dialog is up
            // routes to `refresh_all`, which rebuilds the trash listing and
            // clears the selection; `trash_targets()` would then fall through
            // to the cursor row and destroy one item, possibly not one of the
            // five the dialog named. Purge has no inverse, so the subject has
            // to be the one that was on screen.
            ConfirmKind::Purge | ConfirmKind::EmptyTrash => {
                let items = self
                    .tab()
                    .trash
                    .as_ref()
                    .map(|view| view.items_for(&confirm.paths))
                    .unwrap_or_default();
                self.trash_purge(items, now);
                return;
            }
        };
        self.track(id, slot, dirs);
    }

    /// `-`, `_`, `Ctrl+-`: link the yanked files into this directory.
    ///
    /// The clipboard is the source, as in yazi — `y` then `-` is the gesture,
    /// and linking "the selection" would mean linking files to themselves.
    fn link(&mut self, kind: Option<LinkKind>, now: Instant) {
        if self.clipboard.is_empty() {
            self.toasts.notice("Nothing yanked to link", now);
            return;
        }
        let (cwd, paths) = (self.cwd(), self.clipboard.paths.clone());
        self.link_into(paths, cwd, kind, now);
    }

    /// The same, for a set of files and a directory that were both named by a
    /// drop rather than by the clipboard and the cursor (PLAN §7.1).
    fn link_into(
        &mut self,
        paths: Vec<PathBuf>,
        cwd: PathBuf,
        kind: Option<LinkKind>,
        now: Instant,
    ) {
        let mut made = 0;
        let mut failure = None;
        for target in &paths {
            let Some(name) = target.file_name() else {
                continue;
            };
            let link = cwd.join(name);
            let result = match kind {
                Some(kind) => df_core::ops::symlink(target, &link, kind).map(|_| ()),
                None => df_core::ops::hardlink(target, &link),
            };
            match result {
                Ok(()) => {
                    made += 1;
                    // One record per link: `OpRecord::Link` describes a single
                    // one, so `u` takes them back one at a time.
                    if let Ok(fingerprint) = Fingerprint::of(&link) {
                        self.journal.record(OpRecord::Link {
                            link,
                            target: kind.map(|_| target.clone()),
                            fingerprint,
                        });
                    }
                }
                Err(e) => failure = Some(e.to_string()),
            }
        }
        match (made, failure) {
            (0, Some(error)) => self.toasts.error(error, now),
            (n, _) => {
                self.toasts
                    .undo(format!("Linked {}", plural(n, "item", "items")), now);
                self.rescan(&cwd.clone(), now);
            }
        }
    }

    /// `u` / `Ctrl+Shift+z`.
    ///
    /// Synchronous: an undo is usually a rename back, and the one case that is
    /// not — deleting what a big copy created — is the price of the journal
    /// staying a plain `&mut` stack rather than something a worker can hold.
    /// (Noted as a deferral; moving it to the pool needs a shareable journal.)
    fn undo(&mut self, now: Instant) {
        if self.journal.is_empty() {
            self.toasts.notice("Nothing to undo", now);
            return;
        }
        match self.journal.undo(&TaskCtx::detached()) {
            Ok(report) => {
                // The refusal *and* the success are the user's words: df-core
                // writes these to be read, so they are shown verbatim.
                self.toasts.notice(report.description.clone(), now);
                for dir in Self::affected(&report.touched, None) {
                    self.rescan(&dir, now);
                }
                self.refresh_all(now);
            }
            Err(e) => self.toasts.error(e.to_string(), now),
        }
    }

    // ── Opening (PLAN §6) ───────────────────────────────────────────────────

    /// `o` / `Enter`: the first opener rule that matches.
    fn open_hovered(&mut self, now: Instant) {
        let Some(entry) = self.tab().cwd.dir.cursor_entry().cloned() else {
            return;
        };
        // A directory is *entered*, not launched: `Enter` on a folder has meant
        // "go in" since before file managers had opener rules. The launchers a
        // directory does have (a Zed workspace, a terminal here) are on `O`.
        if entry.is_dir() {
            let path = entry.path.clone();
            self.navigate(path, now);
            return;
        }
        // PLAN §7.6's download-on-open: a remote file has no local path for an
        // opener to take, so it is fetched first and the opener runs on the
        // temp file (which the session's ledger removes on quit).
        if let Some(at) = crate::remote::at_of(&entry.path) {
            self.open_remote(at, entry.name.clone(), now);
            return;
        }
        let choices = open::choices_for(&self.config, &entry);
        let Some(choice) = choices.first().cloned() else {
            self.toasts
                .notice(format!("No opener rule matches {}", entry.name), now);
            return;
        };
        self.launch(&choice, self.targets(), now);
    }

    /// `O` / `Shift+Enter`: the picker, anchored to the row it is about.
    fn open_picker(&mut self, now: Instant) {
        let Some(entry) = self.tab().cwd.dir.cursor_entry().cloned() else {
            return;
        };
        let choices = open::choices_for(&self.config, &entry);
        if choices.is_empty() {
            self.toasts
                .notice(format!("No opener rule matches {}", entry.name), now);
            return;
        }
        let paths = self.targets();
        self.picker = Some(Picker::new(choices, paths, self.cursor_rect));
        self.sync_context();
    }

    /// Run one opener over `paths`.
    fn launch(&mut self, choice: &open::Choice, paths: Vec<PathBuf>, now: Instant) {
        if let Some(builtin) = choice.builtin() {
            // The one built-in the shipped rules name. Archive walking is
            // Phase 5; saying so is better than a rule that silently does
            // nothing (PLAN §6's "fix the yazi gap").
            log::info!("builtin opener `{builtin}` is not implemented yet");
            self.toasts.notice(
                "This opener cannot extract yet — press o to open the archive",
                now,
            );
            return;
        }
        if choice.block {
            self.run_shell(&choice.command.clone(), paths, true, now);
            return;
        }
        let cwd = self.child_cwd();
        if let Err(e) = open::spawn_detached(&choice.command, &paths, &cwd) {
            self.toasts.error(format!("{}: {e}", choice.name), now);
        }
    }

    /// `;` and `:`, and the blocking openers.
    ///
    /// A blocking command runs **on the pool**, not here: `:` means "wait for
    /// it", and waiting on the UI thread would freeze the window until it
    /// exited. Streaming its output into a panel is deferred; the exit status
    /// comes back as a toast.
    fn run_shell(&mut self, snippet: &str, paths: Vec<PathBuf>, block: bool, now: Instant) {
        let cwd = self.child_cwd();
        if !block {
            match open::spawn_detached(snippet, &paths, &cwd) {
                Ok(()) => self
                    .toasts
                    .notice(format!("{} — started", open::short(snippet)), now),
                Err(e) => self.toasts.error(format!("{e}"), now),
            }
            return;
        }
        let slot: Outcome = Arc::new(std::sync::Mutex::new(None));
        let sink = Arc::clone(&slot);
        let snippet = snippet.to_string();
        let label = open::short(&snippet);
        let dirs = vec![cwd.clone()];
        let job = FnJob::new(
            format!("Shell: {label}"),
            Lane::Micro,
            move |_ctx: &TaskCtx| {
                let result = open::run_blocking(&snippet, &paths, &cwd);
                let outcome = match result {
                    // A non-zero exit is *reported*, not treated as a failed
                    // task: the command ran, and "exit 1" is its answer. Only a
                    // command that could not be started at all is an error.
                    Ok(code) => df_core::ops::OpOutcome {
                        message: open::exit_text(&snippet, code),
                        ..Default::default()
                    },
                    Err(e) => df_core::ops::OpOutcome {
                        message: format!("{}: {e}", open::short(&snippet)),
                        errors: vec![(cwd.clone(), e.to_string())],
                        ..Default::default()
                    },
                };
                match sink.lock() {
                    Ok(mut guard) => *guard = Some(outcome),
                    Err(poisoned) => *poisoned.into_inner() = Some(outcome),
                }
                Ok(())
            },
        );
        let id = self.engine.spawn(job);
        self.track(id, slot, dirs);
    }

    // ── Playback (PLAN §4.3, §6) ────────────────────────────────────────────

    /// The hovered file and what the preview pipeline would call it.
    ///
    /// Classified from the **name** rather than from the preview pane's answer,
    /// which is a debounced round trip through a worker: `→ ↓ l` typed at speed
    /// would otherwise find `l` inert on a file whose preview had not landed
    /// yet, and "the transport keys are always the transport keys" is the whole
    /// contract (PLAN §4.3). The name is what yazi's own opener rules go on and
    /// it is a pure function, so this is also what makes `media_hovered`
    /// testable.
    fn hovered_kind(&self) -> Option<(PathBuf, PreviewKind)> {
        if self.tab().archive.is_some() || self.tab().remote.is_some() {
            // Inside an archive, and on a remote service, there is no file to
            // decode: the transport keys are inert and the `MediaHovered`
            // bindings are simply not there — which is how PLAN §4.3's reserved
            // keys stay reserved without a special case in the router. (The
            // trash *is* files on this machine, so it keeps its transport.)
            return None;
        }
        let entry = self.tab().cwd.dir.cursor_entry()?;
        let mime = df_core::fs::mime::hint_for_name(&entry.name);
        Some((entry.path.clone(), df_core::preview::kind_for(entry, mime)))
    }

    /// Is the cursor on something the transport could act on?
    fn media_hovered(&self) -> bool {
        self.hovered_kind()
            .is_some_and(|(_, kind)| is_temporal(&kind))
    }

    /// Keep the controller pointed at the hovered file (PLAN §10's mount /
    /// teardown).
    ///
    /// Called once per frame *after* the keys are routed, like the preview's
    /// own `sync`, so a held `↓` mounts where it stopped rather than at every
    /// row it passed.
    fn sync_playback(&mut self, now: Instant) {
        let hovered = self
            .hovered_kind()
            .filter(|(_, kind)| is_temporal(kind))
            .map(|(path, _)| path);

        match hovered {
            Some(path) => {
                if self.player.as_ref().and_then(Player::path) == Some(path.as_path()) {
                    // Already mounted — including after a flick away and back
                    // inside the grace, which is the whole point of it. The
                    // transport is left exactly as the last key left it.
                    if let Some(player) = self.player.as_mut() {
                        player.stay();
                    }
                } else if let Some((_, probed)) = self.probes.iter().find(|(p, _)| *p == path) {
                    match probed.clone() {
                        // A file dv-media can open, and something in it to play.
                        Some(info) if info.playable() => self.mount(&path, &info, now),
                        // A `.mp4` that is really a text file, or a download
                        // that stopped half way: no transport, and the preview
                        // pane keeps whatever it was already showing.
                        _ => {
                            if let Some(player) = &mut self.player {
                                player.leave(now);
                            }
                        }
                    }
                } else {
                    if self.probing.as_deref() != Some(path.as_path()) {
                        self.prober.probe(&path);
                        self.probing = Some(path.clone());
                    }
                    if let Some(player) = &mut self.player {
                        player.leave(now);
                    }
                }
            }
            // Off a playable file entirely: the sound stops now, and the source
            // itself goes when the grace runs out.
            None => {
                if let Some(player) = &mut self.player {
                    player.leave(now);
                }
            }
        }

        // The teardown, on its own scheduled wake-up (see `next_deadline`).
        let expired = self
            .player
            .as_ref()
            .is_some_and(|player| player.grace_expired(now));
        if expired {
            self.close_player();
        }
    }

    /// Build the controller if this is the first playable file, and point it at
    /// `path` — paused, on its first frame.
    fn mount(&mut self, path: &Path, info: &TemporalInfo, now: Instant) {
        if self.player.is_none() {
            let Some(gfx) = &self.gfx else {
                // No device yet: the cursor is on a clip before the window has
                // been mapped. The next frame's sync mounts it.
                return;
            };
            let (device, queue) = (gfx.device.clone(), gfx.queue.clone());
            self.player = Some(Player::new(&device, &queue, now));
        }
        if let Some(player) = &mut self.player {
            player.open(path, info, now);
        }
    }

    /// Drop the source and the frame texture it registered. Kept in one place
    /// because forgetting the `free_texture` half leaks a video-sized texture
    /// per clip.
    fn close_player(&mut self) {
        let Some(gfx) = &mut self.gfx else { return };
        if let Some(player) = &mut self.player {
            player.close(&mut gfx.renderer);
        }
    }

    /// The transport, when there is one pointed at the file under the cursor.
    ///
    /// Every transport command goes through this rather than through
    /// `self.player` directly: a `k` that reached a controller still holding
    /// the *previous* file would play a file nobody is looking at.
    fn transport(&mut self) -> Option<&mut Player> {
        let hovered = self.tab().cwd.dir.cursor_entry().map(|e| e.path.clone())?;
        let player = self.player.as_mut()?;
        (player.path() == Some(hovered.as_path())).then_some(player)
    }

    // ── Commands ────────────────────────────────────────────────────────────

    /// Turn queued keystrokes into commands and run them.
    ///
    /// `page` is how many rows are on screen, which is what `Ctrl+f` and
    /// `Ctrl+d` are measured in — hence keys being routed mid-frame, once the
    /// panes have been laid out.
    fn route_keys(&mut self, page: usize, now: Instant) {
        // The `when` predicates, rebuilt from the two things that decide them:
        // which pane has the keyboard, and whether the cursor is on something
        // playable (PLAN §2.1, §4.3). Recomputed per keystroke rather than per
        // frame, because a keystroke can move the cursor onto a clip and the
        // *next* keystroke in the same frame has to see that.
        for press in std::mem::take(&mut self.pending_keys) {
            let flags = self.focus.flags(self.media_hovered());
            self.key_repeat = press.repeat;
            // The chord is the binding; the text is the fallback for a key the
            // chord table cannot name — a composed character, a layout's own
            // letter — which still has to be typeable into a prompt.
            let chord = press.chord.or_else(|| {
                press
                    .text
                    .as_deref()
                    .and_then(|text| text.chars().next())
                    .and_then(Chord::from_char)
            });
            let Some(chord) = chord else { continue };
            // The context menu is the nearest surface to the user and takes
            // the keyboard whole, above even a modal card — it is the most
            // recent thing they asked for. See [`App::menu_key`] for why its
            // keys are matched literally.
            if self.menu.as_ref().is_some_and(Menu::live) {
                self.menu_key(chord, now);
                continue;
            }
            // A prompt swallows every key: df-core's editor decides what each
            // one means, including which ones are text (PLAN §4.2).
            if self.prompt.is_some() {
                self.prompt_key(chord, now);
                continue;
            }
            // A modal surface is matched against its own context **alone**.
            // Merely pushing `Confirm` onto the browser's stack would leave
            // `Files` reachable underneath it, and a `d` typed into a delete
            // confirmation would queue a second trash. A dialog that is asking
            // "are you sure" must not also be a file manager.
            if self.overlay_open() {
                self.overlay_key(chord, page, now);
                continue;
            }
            match self
                .keymap
                .dispatch(&mut self.keys, &self.context, flags, chord, now)
            {
                Dispatch::Match(command) => self.run(command, page, now),
                // The chord is held: remember what could finish it, in df-core's
                // declaration order (PLAN §4), for the card to draw once it is
                // due. Nothing is shown yet — that is [`WhichKey`]'s decision.
                Dispatch::Pending { continuations, .. } => {
                    self.which_rows = continuations
                        .iter()
                        .map(|c| (c.label(), c.description.clone()))
                        .collect();
                }
                Dispatch::NoMatch => log::trace!("unbound: {}", chord.label()),
            }
        }
    }

    // ── The modal surfaces: dialog, picker, task panel ──────────────────────

    fn overlay_open(&self) -> bool {
        self.dialog.is_some()
            || self.picker.is_some()
            || self.panel.is_some()
            || self.spot.is_some()
            || self.finder.is_some()
            || self.search.is_some()
    }

    /// The context an open surface is matched in. Never stacked on `Files`:
    /// see [`App::route_keys`].
    fn overlay_stack(&self) -> ContextStack {
        let context = if self.dialog.is_some() {
            Context::Confirm
        } else if self.finder.is_some() {
            Context::Palette
        } else if self.search.is_some() {
            // The search panel takes `[pick]`'s five keys — Esc, Ctrl+c, Enter
            // and the two arrows — because that is the same "choose one of
            // these" vocabulary, and a sixth context whose table would be an
            // exact copy of `[pick]`'s is a sixth table to keep in step.
            Context::Pick
        } else if self.mounts.is_some() {
            // The disks card is a "choose one of these" surface, so it takes
            // `[pick]`'s vocabulary — Esc, Enter and the two arrows — rather
            // than growing a table that would be a copy of it.
            Context::Pick
        } else if self.picker.is_some() {
            Context::Pick
        } else if self.spot.is_some() {
            Context::Spot
        } else {
            Context::Tasks
        };
        ContextStack::with(&[context])
    }

    fn overlay_key(&mut self, chord: Chord, page: usize, now: Instant) {
        // The rename card is a grid of line editors, so it takes the keystroke
        // *before* the registry gets a look at it — otherwise typing `d` into a
        // name would be the `[confirm]` table's `d`.
        if self.bulk_key(chord, now) {
            return;
        }
        if self.overlay_literal(chord, now) {
            return;
        }
        let stack = self.overlay_stack();
        let dispatch = self
            .keymap
            .dispatch(&mut self.keys, &stack, WhenFlags::LIST, chord, now);
        let Dispatch::Match(command) = dispatch else {
            // The two overlays with a field in them take every key the
            // registry did not claim — which is the same rule the bottom-bar
            // prompt follows, and the reason a `q` typed into a search is a
            // `q` and not a quit.
            self.overlay_text(chord);
            return;
        };
        use Command as C;
        match command {
            // Only the overlay vocabulary is honoured. `Global` is still under
            // the stack — that is where `Esc` lives — but a `Ctrl+p` palette or
            // a `~` help sheet opening *behind* a modal card would be a second
            // surface nobody asked for.
            C::Escape | C::OverlayClose => self.close_overlay(now),
            C::OverlaySubmit => self.submit_overlay(page, now),
            C::OverlayPrev => self.overlay_move(-1),
            C::OverlayNext => self.overlay_move(1),
            C::TaskInspect => {
                if let Some(panel) = &mut self.panel {
                    panel.inspect = !panel.inspect;
                }
            }
            C::TaskCancel => self.cancel_selected_task(now),
            // The spot's own two: `←`/`→` walk the directory with the card
            // following, which is what makes it a panel rather than a dialog.
            C::SpotSwipePrev | C::SpotSwipeNext => {
                self.swipe_spot(if command == C::SpotSwipeNext { 1 } else { -1 })
            }
            // The card's own `c c`: the focused row's value, not the file's
            // path — the browser's `c c` is the one that copies that.
            C::SpotCopyCell => self.copy_spot_cell(now),
            other => log::trace!("`{}` is not an overlay key", other.id()),
        }
    }

    /// The keys a surface handles itself, because df-core's keymap has no row
    /// for them.
    ///
    /// Two of these are gaps in the shipped `[confirm]`/`[tasks]` tables rather
    /// than deliberate omissions: the conflict resolver's three answers, and
    /// pausing a task. They are matched literally here and are noted so the
    /// keymap can grow rows for them without this code changing shape.
    fn overlay_literal(&mut self, chord: Chord, now: Instant) -> bool {
        let plain = chord.mods.is_none() || chord.mods == df_core::keymap::Mods::SHIFT;
        // The disks card's two extra verbs. Matched literally for the same
        // reason the conflict resolver's answers were: `[pick]` is the shared
        // "choose one of these" table and it has no row for ejecting a drive.
        if self.mounts.is_some() && chord.mods.is_none() {
            match chord.key {
                Key::Char('e') => {
                    self.eject_selected(now);
                    return true;
                }
                Key::Char('u') => {
                    self.unmount_selected(now);
                    return true;
                }
                _ => {}
            }
        }
        if let Some(Dialog::Conflict(dialog)) = &mut self.dialog {
            match chord.key {
                Key::Char(c) if plain => {
                    if let Some(action) = ConflictDialog::action_for_key(c) {
                        dialog.set_action(action);
                        return true;
                    }
                    if c == 'a' {
                        dialog.toggle_apply_all();
                        return true;
                    }
                }
                // `←`/`→` walk the three answers, which is what they mean on a
                // row of buttons everywhere else.
                Key::ArrowLeft if plain => {
                    dialog.cycle_action(-1);
                    return true;
                }
                Key::ArrowRight if plain => {
                    dialog.cycle_action(1);
                    return true;
                }
                _ => {}
            }
        }
        // `Ctrl+s` stops a running search without closing the panel — PLAN
        // §7.2's cancel. It is matched literally because df-core's `[pick]`
        // table has no row for it: the binding lives in `[files]`, which the
        // panel deliberately does not stack on (see `overlay_stack`). A
        // `[pick]` row for `cancel-search` would put it on the help sheet,
        // which is where it belongs.
        if let Some(search) = &mut self.search {
            if chord == Chord::ctrl(Key::Char('s')) {
                search.cancel();
                return true;
            }
        }
        if self.panel.is_some() && plain && chord.key == Key::Char('p') {
            self.pause_selected_task(now);
            return true;
        }
        // The spot's two keys df-core's `[spot]` table has no row for, and the
        // reason each is here rather than there:
        //
        // `Space` acts on the focused row — it is the *same* "do the thing
        // under the cursor" `Enter` is, and binding a second row for it in a
        // context where `Enter` is already the verb would put two identical
        // lines on the help sheet.
        //
        // `Shift+←`/`Shift+→` move the permission selection, because the plain
        // arrows are the file swipe and that is the binding the keymap
        // documents (PLAN §4.1's note on `h`/`l`).
        if self.spot.is_some() {
            if plain && chord.key == Key::Space {
                self.spot_action(now);
                return true;
            }
            let shifted = chord.mods == df_core::keymap::Mods::SHIFT;
            if shifted && matches!(chord.key, Key::ArrowLeft | Key::ArrowRight) {
                let delta = if chord.key == Key::ArrowRight { 1 } else { -1 };
                if let Some(spot) = &mut self.spot {
                    spot.move_bit(delta);
                }
                return true;
            }
        }
        false
    }

    /// A key the registry did not claim, offered to whichever overlay has a
    /// field in it.
    ///
    /// df-core's [`InputBuffer`] decides what each key *means*, including which
    /// ones are text — the same editor the rename prompt and the filter use
    /// (PLAN §4.2), so there is one set of Unicode edge cases in this program
    /// rather than three.
    fn overlay_text(&mut self, chord: Chord) {
        if self.finder.is_some() {
            let consumed = self
                .finder
                .as_mut()
                .map(|finder| finder.buffer.feed(chord))
                .is_some_and(|event| matches!(event, InputEvent::Consumed));
            if consumed {
                self.finder_changed();
            }
            return;
        }
        let Some(search) = &mut self.search else {
            return;
        };
        if matches!(search.buffer.feed(chord), InputEvent::Consumed) {
            search.changed(Instant::now());
        }
    }

    fn overlay_move(&mut self, delta: isize) {
        if let Some(finder) = &mut self.finder {
            finder.move_cursor(delta);
            return;
        }
        if let Some(search) = &mut self.search {
            search.move_cursor(delta);
            self.search_follow = true;
            return;
        }
        match &mut self.dialog {
            Some(Dialog::Confirm(confirm)) => {
                confirm.scroll_by(delta);
                return;
            }
            Some(Dialog::Conflict(dialog)) => {
                dialog.move_cursor(delta);
                return;
            }
            Some(Dialog::Bulk(bulk)) => {
                bulk.step(delta);
                return;
            }
            None => {}
        }
        if let Some(card) = &mut self.mounts {
            card.move_cursor(delta);
            return;
        }
        if let Some(picker) = &mut self.picker {
            picker.move_cursor(delta);
            return;
        }
        if let Some(spot) = &mut self.spot {
            spot.move_cursor(delta);
            return;
        }
        let rows = self.task_rows();
        if let Some(panel) = &mut self.panel {
            panel.move_cursor(delta, rows.len());
        }
    }

    /// `Enter` on whatever is up.
    fn submit_overlay(&mut self, page: usize, now: Instant) {
        if self.finder.is_some() {
            self.finder_submit(page, now);
            return;
        }
        if self.search.is_some() {
            self.search_submit(now);
            return;
        }
        if self.mounts.is_some() {
            self.mount_action(now);
            return;
        }
        if self.spot.is_some() {
            self.spot_action(now);
            return;
        }
        if let Some(Dialog::Confirm(_)) = &self.dialog {
            let Some(Dialog::Confirm(confirm)) = self.dialog.take() else {
                return;
            };
            self.sync_context();
            self.run_confirm(confirm, now);
            return;
        }
        if let Some(Dialog::Conflict(dialog)) = &mut self.dialog {
            match dialog.apply() {
                Step::Continue => {}
                Step::NeedName(suggested) => {
                    let buffer = InputBuffer::for_rename_stem(&suggested);
                    self.open_prompt_with(PromptKind::ConflictRename, buffer);
                }
                Step::Settled => {
                    let plan = match self.dialog.take() {
                        Some(Dialog::Conflict(dialog)) => Some(dialog.plan),
                        other => {
                            self.dialog = other;
                            None
                        }
                    };
                    self.sync_context();
                    if let Some(plan) = plan {
                        self.spawn_paste(plan, now);
                    }
                }
            }
            return;
        }
        if let Some(picker) = self.picker.take() {
            self.sync_context();
            if let Some(choice) = picker.chosen().cloned() {
                self.launch(&choice, picker.paths.clone(), now);
            }
        }
    }

    /// `Esc`, and every other way of saying "not this".
    fn close_overlay(&mut self, now: Instant) {
        // A prompt the dialog opened is *inside* it, so the dialog's own Esc
        // takes that down first and the card stays up. This is the §4.1 ladder's
        // "dialog before prompt" read the only way it can happen.
        if self.prompt.is_some() && self.dialog.is_some() {
            self.prompt = None;
            self.sync_context();
            return;
        }
        match self.dialog.take() {
            Some(Dialog::Conflict(_)) => {
                // Cancelling a conflict cancels the whole paste (PLAN §5): the
                // clipboard is untouched, so `p` starts it again.
                self.toasts.notice("Paste cancelled", now);
            }
            Some(Dialog::Bulk(_)) => {
                self.toasts.notice("Rename cancelled", now);
            }
            Some(Dialog::Confirm(_)) | None => {}
        }
        self.picker = None;
        self.panel = None;
        self.finder = None;
        // Dropping the search kills its process — see `search::Running`'s
        // `Drop`. Closing the panel must not leave an `rg` walking a home
        // directory for a list nobody will ever see.
        self.search = None;
        // Dropping the panel stops its hasher: see `spot::Hasher`'s `Drop`.
        self.spot = None;
        // The card goes; the worker stays. A session that opens the disks card
        // three times should authenticate to the system bus once.
        self.mounts = None;
        self.sync_context();
    }

    // ── The fuzzy card: `Ctrl+p`, `z`, `Z` (PLAN §4.4, §7.2) ────────────────

    /// `Ctrl+p`. Every command that is live right now, plus the places and the
    /// tabs.
    fn open_palette(&mut self) {
        let rows = self.palette_rows();
        self.finder = Some(Finder::new(Source::Commands, rows));
        self.sync_context();
    }

    /// What the palette lists.
    ///
    /// Read out of the registry, never out of a hand-written table — PLAN §4's
    /// "one registry feeds four surfaces", and this is the fourth. A command
    /// whose `when` predicate says it is not reachable right now is **absent**,
    /// not greyed: a palette is a list of things you can do.
    fn palette_rows(&self) -> Vec<finder::Row> {
        let flags = self.focus.flags(self.media_hovered());
        let mut rows: Vec<finder::Row> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for binding in self.keymap.active_bindings(&ContextStack::browser(), flags) {
            // A command bound twice — `~` and `F1` are both the help sheet —
            // is one thing you can do, and it appears once, under the first
            // chord the registry declares for it.
            if !seen.insert(binding.command.id()) {
                continue;
            }
            // The goto bookmarks come back below as places, with their paths
            // showing, which is more use than "Go to Work — g w".
            if matches!(binding.command, Command::Goto(_)) {
                continue;
            }
            rows.push(finder::Row {
                label: binding.description.clone(),
                detail: binding.label(),
                kind: finder::Kind::Command,
                choice: Choice::Run(binding.command),
            });
        }
        // The commands a person has actually used, first.
        finder::by_recency(&mut rows, &self.mru);
        // "Empty trash", which is reachable only while the trash is on screen
        // and therefore has no chord to be found under: a key bound to
        // "destroy everything I have deleted" is a key somebody presses by
        // accident, and this one asks first *and* has to be typed for.
        if self.tab().trash.is_some() {
            let n = self
                .tab()
                .trash
                .as_ref()
                .map(|v| v.items.len())
                .unwrap_or(0);
            rows.push(finder::Row {
                label: "Empty trash".to_string(),
                detail: plural(n, "item", "items"),
                kind: finder::Kind::Command,
                choice: Choice::Run(Command::EmptyTrash),
            });
        }
        // The view toggle, which has no registry row to be found under — see
        // `Choice::ToggleView`.
        rows.push(finder::Row {
            label: if self.is_grid() {
                "Show this folder as a list".to_string()
            } else {
                "Show this folder as a grid".to_string()
            },
            detail: String::new(),
            kind: finder::Kind::View,
            choice: Choice::ToggleView,
        });
        // Then the places: bookmarks, then where this tab has been.
        rows.extend(finder::merge_places(
            self.tab().history.back_stack(),
            &self.config.goto,
            &[],
            home().as_deref(),
        ));
        // And the open tabs, when there is more than one to choose between.
        if self.tabs.len() > 1 {
            for (n, tab) in self.tabs.iter().enumerate() {
                rows.push(finder::Row {
                    label: format!("Tab {}: {}", n + 1, file_name(tab.cwd.path())),
                    detail: finder::shorten_home(tab.cwd.path(), home().as_deref()),
                    kind: finder::Kind::Tab,
                    choice: Choice::Tab(n),
                });
            }
        }
        rows
    }

    /// `z` and `Z`.
    fn open_jump(&mut self, source: Source) {
        let rows = self.jump_rows(source, "");
        self.finder = Some(Finder::new(source, rows));
        self.sync_context();
    }

    /// The place list for a jump overlay.
    ///
    /// `z` merges everything and lets the fuzzy ranker sort it out; `Z` asks
    /// zoxide, whose own ordering is the answer and must survive (see
    /// [`Finder::ranking_query`]).
    fn jump_rows(&mut self, source: Source, query: &str) -> Vec<finder::Row> {
        let home = home();
        match source {
            Source::Zoxide => {
                let dirs = self.zoxide_db().to_vec();
                let matches = df_core::zoxide::query(&dirs, query, unix_now());
                finder::zoxide_rows(&matches, home.as_deref())
            }
            _ => {
                let dirs = self.zoxide_db().to_vec();
                finder::merge_places(
                    self.tab().history.back_stack(),
                    &self.config.goto,
                    &dirs,
                    home.as_deref(),
                )
            }
        }
    }

    /// zoxide's database, read once per session.
    fn zoxide_db(&mut self) -> &[df_core::zoxide::ZoxideDir] {
        self.zoxide.get_or_insert_with(df_core::zoxide::load)
    }

    /// A keystroke landed in the card: re-rank, and for `Z` re-ask zoxide.
    fn finder_changed(&mut self) {
        let Some(finder) = &self.finder else { return };
        let source = finder.source;
        if source == Source::Zoxide {
            let query = finder.query().to_string();
            let rows = self.jump_rows(source, &query);
            if let Some(finder) = &mut self.finder {
                finder.set_pool(rows);
                finder.cursor = 0;
                finder.first = 0;
            }
            return;
        }
        if let Some(finder) = &mut self.finder {
            finder.requery();
        }
    }

    /// `Enter` on the card.
    fn finder_submit(&mut self, page: usize, now: Instant) {
        let Some(finder) = &self.finder else { return };
        let Some(choice) = finder.chosen().map(|row| row.choice.clone()) else {
            return;
        };
        self.finder = None;
        self.sync_context();
        match choice {
            // Straight back through `run`, so there is exactly one
            // implementation of every command and the palette cannot drift
            // from the key that runs it.
            Choice::Run(command) => {
                self.mru.touch(command.id());
                self.run(command, page, now);
            }
            Choice::Cd(path) => self.jump_to(path, now),
            Choice::Tab(n) => {
                if self.tabs.switch_to(n, now) {
                    self.tab_changed(now);
                }
            }
            Choice::ToggleView => self.toggle_view(now),
        }
    }

    // ── The mount manager: `M` (PLAN §7.4) ──────────────────────────────────

    /// The udisks2 worker, started the first time the card is opened.
    fn udisks(&mut self) -> &crate::mounts::Mounts {
        let waker = self.waker.named("udisks");
        self.udisks
            .get_or_insert_with(|| crate::mounts::Mounts::start(Arc::new(move || waker.wake())))
    }

    /// `M`: open the card and ask for the listing.
    fn open_mounts(&mut self) {
        self.mounts = Some(crate::mounts::Card::new());
        self.udisks().ask(crate::mounts::Request::List);
        self.sync_context();
    }

    /// `Enter` on a row: mount it, or — if it is already mounted — go there.
    ///
    /// The two on one key, because they are the same intention. What a person
    /// wants from a disk in a file manager is to be *in* it; mounting is the
    /// step that has to happen first when it has not happened yet, and pressing
    /// `Enter` twice on a fresh USB stick doing both is the shortest true
    /// description of the job.
    fn mount_action(&mut self, now: Instant) {
        let Some(card) = &self.mounts else { return };
        if card.busy.is_some() {
            // One call at a time: two mounts of the same device is one of them
            // failing with `AlreadyMounted`.
            return;
        }
        let Some(device) = card.selected().cloned() else {
            return;
        };
        match &device.mount {
            Some(path) => {
                let path = path.clone();
                self.close_overlay(now);
                self.jump_to(path, now);
            }
            None => {
                if let Some(card) = &mut self.mounts {
                    card.busy = Some(device.object.clone());
                }
                self.udisks()
                    .ask(crate::mounts::Request::Mount(device.object));
            }
        }
    }

    /// `u` on a mounted row: put it away.
    fn unmount_selected(&mut self, now: Instant) {
        let Some(device) = self
            .mounts
            .as_ref()
            .filter(|card| card.busy.is_none())
            .and_then(|card| card.selected())
            .cloned()
        else {
            return;
        };
        if !device.is_mounted() {
            self.toasts
                .notice(format!("{} is not mounted", device.label), now);
            return;
        }
        if let Some(card) = &mut self.mounts {
            card.busy = Some(device.object.clone());
        }
        self.udisks()
            .ask(crate::mounts::Request::Unmount(device.object));
    }

    /// `e`: eject the whole drive, which is what "safely remove" means.
    fn eject_selected(&mut self, now: Instant) {
        let Some(device) = self
            .mounts
            .as_ref()
            .filter(|card| card.busy.is_none())
            .and_then(|card| card.selected())
            .cloned()
        else {
            return;
        };
        let Some(drive) = device.drive.clone().filter(|_| device.ejectable) else {
            self.toasts
                .notice(format!("{} cannot be ejected", device.label), now);
            return;
        };
        if let Some(card) = &mut self.mounts {
            card.busy = Some(device.object.clone());
        }
        self.udisks().ask(crate::mounts::Request::Eject(drive));
    }

    /// Take whatever the worker has said. Returns whether anything changed.
    fn poll_mounts(&mut self, now: Instant) -> bool {
        let replies = match &self.udisks {
            Some(udisks) => udisks.drain(),
            None => return false,
        };
        if replies.is_empty() {
            return false;
        }
        let mut refresh = false;
        for reply in replies {
            if let Some(card) = &mut self.mounts {
                card.busy = None;
            }
            match reply {
                crate::mounts::Reply::Devices(devices) => {
                    if let Some(card) = &mut self.mounts {
                        card.update(devices);
                    }
                }
                crate::mounts::Reply::Mounted(path) => {
                    self.toasts
                        .notice(format!("Mounted at {}", path.display()), now);
                    refresh = true;
                }
                crate::mounts::Reply::Unmounted => {
                    self.toasts.notice("Unmounted", now);
                    refresh = true;
                }
                crate::mounts::Reply::Ejected => {
                    self.toasts.notice("Safe to remove", now);
                    refresh = true;
                }
                crate::mounts::Reply::Failed(message) => {
                    // The card stays up: the failure is about one row, and
                    // closing the surface would take the other disks away too.
                    self.toasts.error(message, now);
                    if self.mounts.as_ref().is_some_and(|card| card.loading) {
                        // …unless nothing ever arrived, in which case there is
                        // no card to stay up.
                        self.mounts = None;
                        self.sync_context();
                    }
                }
            }
        }
        if refresh && self.mounts.is_some() {
            self.udisks().ask(crate::mounts::Request::List);
        }
        true
    }

    // ── The selection basket (PLAN §7.1) ────────────────────────────────────

    /// `b`: toss the selection — or the row under the cursor — in, or back out.
    fn toss_basket(&mut self, now: Instant) {
        let paths = self.targets();
        if paths.is_empty() {
            self.toasts.notice("Nothing to put in the basket", now);
            return;
        }
        let tossed = self.basket.toss(&paths);
        self.clamp_basket();
        let message = if tossed.full {
            format!(
                "The basket is full at {} — took {}",
                crate::basket::CAPACITY,
                plural(tossed.added, "file", "files")
            )
        } else if tossed.removed > 0 {
            format!(
                "Took {} out of the basket",
                plural(tossed.removed, "file", "files")
            )
        } else {
            format!(
                "{} in the basket — {} in all",
                plural(tossed.added, "file", "files"),
                self.basket.len()
            )
        };
        self.toasts.notice(message, now);
        // Advance the cursor like `Space` does: `b b b` down a listing is the
        // gesture, and stopping to move the cursor between each would make it
        // six keystrokes instead of three.
        if self.tab().cwd.dir.selected_count() == 0 {
            self.dir().move_cursor(1);
        }
    }

    /// `B`, and the palette's "Show the selection basket".
    fn show_basket(&mut self, now: Instant) {
        if self.basket.is_empty() {
            self.toasts
                .notice("The basket is empty — press b to put files in it", now);
            return;
        }
        self.basket_open = !self.basket_open;
        if self.basket_open {
            self.prune_basket(now);
        }
    }

    /// Drop paths that are gone, and say how many. Called when the tray opens:
    /// that is the moment the list is about to be read, and so the moment it
    /// has to be true.
    fn prune_basket(&mut self, now: Instant) {
        let gone = self.basket.prune();
        self.clamp_basket();
        if gone > 0 {
            self.toasts.notice(
                format!("{} no longer there", plural(gone, "file is", "files are")),
                now,
            );
        }
    }

    /// Keep the tray's scroll inside the list it is about, and close it when
    /// there is nothing left to show.
    fn clamp_basket(&mut self) {
        let len = self.basket.len();
        if len == 0 {
            self.basket_open = false;
            self.basket_first = 0;
            return;
        }
        self.basket_first = self
            .basket_first
            .min(len.saturating_sub(crate::basket::ROWS.min(len)));
    }

    /// A click on a tray row: go to the file, wherever it is.
    ///
    /// The basket's whole point is that its contents are somewhere else, so a
    /// row is a *link* — the pane navigates to the directory and the cursor
    /// lands on the file.
    fn reveal(&mut self, path: &Path, now: Instant) {
        let Some(parent) = path.parent().map(Path::to_path_buf) else {
            return;
        };
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            return;
        };
        if parent != self.cwd() {
            self.navigate(parent.clone(), now);
        }
        // The scan may not have landed yet, so this is the same deferred
        // placement `--cwd-file` uses.
        if !self.tabs.active_mut().cwd.dir.cursor_to_name(&name) {
            self.start_cursor = Some((parent, name));
        }
    }

    /// Go to a directory a jump overlay named.
    ///
    /// A directory that has gone away says so rather than leaving you looking
    /// at the old one wondering whether the key worked — zoxide's database and
    /// a tab's history both outlive the directories in them.
    fn jump_to(&mut self, path: PathBuf, now: Instant) {
        if !path.is_dir() {
            self.toasts
                .error(format!("{} is not there any more", path.display()), now);
            return;
        }
        self.navigate(path, now);
    }

    // ── The search panel: `s` / `S` (PLAN §7.2) ─────────────────────────────

    /// `s` and `S`. Opens empty: the first keystroke arms the debounce and the
    /// process starts 150 ms later.
    fn open_search(&mut self, mode: search::Mode) {
        let notify: df_core::fs::Notifier = {
            let waker = self.waker.named("search");
            Arc::new(move || waker.wake())
        };
        self.search = Some(Search::new(
            mode,
            self.tab().cwd.path().to_path_buf(),
            self.mgr.show_hidden,
            notify,
        ));
        self.sync_context();
    }

    /// `Enter` on a result: go to the file's directory, put the cursor on it,
    /// and close.
    ///
    /// Not "open it" — a file manager's answer to "I found it" is to *be
    /// there*, with the file under the cursor and every key that acts on a file
    /// pointed at it. `Enter` again opens it, which is one more keystroke and
    /// the one you would have pressed anyway.
    fn search_submit(&mut self, now: Instant) {
        let Some(hit) = self.search.as_ref().and_then(Search::chosen) else {
            return;
        };
        let (path, name) = (hit.path.clone(), file_name(&hit.path));
        let Some(dir) = path.parent().map(Path::to_path_buf) else {
            return;
        };
        self.search = None;
        self.sync_context();
        if dir != self.tab().cwd.path() {
            self.navigate(dir, now);
            // The scan is asynchronous, so the cursor cannot land yet — the
            // same problem `delightfile <file>` has on the command line, and
            // the same answer.
            self.start_cursor = Some((path, name));
        } else if !self.dir().cursor_to_name(&name) {
            self.toasts
                .error(format!("{name} is not in this folder any more"), now);
        }
    }

    // ── The grid, and the state file it is remembered in (PLAN §2) ──────────

    /// How this directory is drawn. The config has no grid setting, so a
    /// directory nobody has toggled is a list.
    fn view_of(&self, dir: &Path) -> View {
        self.state.view(dir).unwrap_or(View::List)
    }

    fn is_grid(&self) -> bool {
        self.view_of(self.tab().cwd.path()) == View::Grid
    }

    /// Flip this directory between the list and the grid, and remember it.
    fn toggle_view(&mut self, now: Instant) {
        let path = self.tab().cwd.path().to_path_buf();
        let next = self.view_of(&path).toggled();
        // `List` is the default, so it is stored as "no preference" rather than
        // as a record — otherwise every directory anyone ever glanced at in a
        // grid and switched back would live in the state file for ever.
        self.state.set_view(
            path,
            match next {
                View::Grid => Some(View::Grid),
                View::List => None,
            },
        );
        self.state_changed(now);
        // The two geometries count their scroll in different units — list rows
        // one side, rows of tiles the other — so the view *jumps* rather than
        // slides: animating a number from one unit to the other would draw a
        // travel that means nothing. The scrolloff rule puts it where the
        // cursor says, this frame.
        self.tabs
            .active_mut()
            .cwd
            .set_first_over(0, Duration::ZERO, now);
        self.toasts.notice(
            match next {
                View::Grid => "Grid view",
                View::List => "List view",
            },
            now,
        );
    }

    /// The state file has something new in it. Arms the write.
    ///
    /// PLAN §2's contract: df-core's store knows *what* changed and this side
    /// owns *when* it is written. A write per keystroke would be an fsync per
    /// `,` chord; a write only at quit would lose everything to a crash. The
    /// debounce is the middle, and it is one scheduled wake-up rather than a
    /// poll.
    fn state_changed(&mut self, now: Instant) {
        self.state_due.touch(self.state.is_dirty(), now);
    }

    /// Write the state file if its debounce has expired.
    fn tick_state(&mut self, now: Instant) {
        if self.state_due.ready(now) {
            self.flush_state();
        }
    }

    /// Write the state file now — the quit path, where there is no later.
    fn flush_state(&mut self) {
        self.state_due.disarm();
        if let Err(e) = self.state.flush() {
            // A state file that will not write costs the memory of which
            // folders are grids and nothing else, so it is a log line rather
            // than a toast in the user's face on the way out.
            log::warn!("could not write the state file: {e}");
        }
    }

    /// Where the items the list pane will draw are, keyed by path.
    ///
    /// The FLIP's participants, and therefore where the cap on them lives: the
    /// visible window plus [`FLIP_MARGIN`] rows either side, so a re-sort
    /// animates the rows a person can see and the ones just off the edge that
    /// are about to travel into view — and never the ten thousand it cannot.
    fn pane_layout(
        &self,
        content: egui::Rect,
        metrics: Option<&grid::Metrics>,
        scroll_rows: f32,
    ) -> Snapshot {
        let dir = &self.tab().cwd.dir;
        let columns = metrics.map(|m| m.columns).unwrap_or(1);
        let visible = crate::viewport::visible_rows(content.height(), grid::pane_step(metrics));
        let first_row = scroll_rows.floor().max(0.0) as usize;
        let from = first_row.saturating_sub(FLIP_MARGIN) * columns;
        // `visible + 1` for the row half on screen at the bottom.
        let to = ((first_row + visible + 1 + FLIP_MARGIN) * columns).min(dir.len());
        (from..to)
            .filter_map(|index| {
                let entry = dir.row(index)?;
                Some((
                    entry.path.clone(),
                    grid::pane_rect(content, metrics, scroll_rows, index),
                ))
            })
            .collect()
    }

    /// The tiles worth a thumbnail this frame, nearest first.
    fn tile_wants(
        &self,
        content: egui::Rect,
        metrics: &grid::Metrics,
        scroll_rows: f32,
    ) -> Vec<grid::Want> {
        let dir = &self.tab().cwd.dir;
        let window = grid::wanted(
            dir.len(),
            metrics.columns,
            scroll_rows.floor().max(0.0) as usize,
            metrics.visible_rows(content.height()),
        );
        window
            .filter_map(|index| {
                let entry = dir.row(index)?;
                let kind = df_core::preview::kind_for(entry, entry.mime);
                Some(grid::Want {
                    path: entry.path.clone(),
                    // Only a still image is decoded from the file itself; see
                    // `grid::Want::decode_source` for why forty videos are not.
                    decode_source: kind == PreviewKind::Image,
                })
            })
            .collect()
    }

    /// The grid's thumbnail workers, started on first use.
    fn thumbs(&mut self) -> &mut Thumbs {
        if self.thumbs.is_none() {
            let waker = self.waker.named("thumbs");
            let notify: df_core::fs::Notifier = Arc::new(move || waker.wake());
            self.thumbs = Some(Thumbs::start(notify));
        }
        // Just assigned above when it was `None`.
        self.thumbs.as_mut().expect("the pool was just started")
    }

    // ── The spot panel (PLAN §6) ────────────────────────────────────────────

    /// `Tab`: the card about the hovered file, which the same key closes again.
    fn toggle_spot(&mut self) {
        if self.spot.is_some() {
            self.spot = None;
            self.sync_context();
            return;
        }
        let Some(entry) = self.tab().cwd.dir.cursor_entry() else {
            // An empty directory has nothing to spot, and a card about nothing
            // is worse than no card.
            return;
        };
        self.spot = Some(Spot::new(self.spot_facts(entry)));
        self.sync_context();
    }

    /// `←`/`→` in the panel: move the list's cursor and let the card follow.
    ///
    /// The **list** cursor moves, not just the card's subject — closing the
    /// panel must leave you where the panel left you, or the swipe was a lie
    /// about where you are.
    fn swipe_spot(&mut self, delta: isize) {
        if self.spot.is_none() {
            return;
        }
        self.dir().move_cursor(delta);
        let facts = self
            .tab()
            .cwd
            .dir
            .cursor_entry()
            .map(|entry| self.spot_facts(entry));
        if let (Some(spot), Some(facts)) = (&mut self.spot, facts) {
            spot.swipe(facts);
        }
    }

    /// Everything the card knows, from what is already in hand.
    ///
    /// The **sniff** is the one blocking read here: 8 KiB off the front of the
    /// file, on a key press, and it is the same 8 KiB the preview worker read
    /// for this file a moment ago. It is done inline rather than on a worker
    /// because a mime that appears a frame later would make the card reflow
    /// under the pointer (`delightful-ui` §8), and because the alternative —
    /// showing the extension's guess and then correcting it — is the panel
    /// contradicting itself.
    fn spot_facts(&self, entry: &df_core::fs::Entry) -> spot::Facts {
        let mut facts = spot::Facts::from_entry(entry);
        if !entry.is_dir() {
            if let Ok(mime) = df_core::preview::sniff_file(&entry.path, entry.mime) {
                facts.mime = mime.to_string();
            }
        }
        // What ffmpeg already said about this file, if the cursor has been on
        // it. Never a fresh probe: the card is not a reason to open a codec.
        facts.media = self
            .probes
            .iter()
            .find(|(path, _)| *path == entry.path)
            .and_then(|(_, info)| info.as_ref())
            .map(|info| spot::MediaFacts {
                width: info.width,
                height: info.height,
                duration_us: info.duration_us,
                video_codec: info.video_codec.clone(),
                audio_codec: info.audio_codec.clone(),
                sample_rate: info.sample_rate,
            });
        facts
    }

    /// Keep the card's asynchronous halves up to date: git, and the hasher.
    ///
    /// Called once a frame while the panel is open. Both are cheap when there
    /// is nothing new — `Git::ensure` reads a memoised map and `Spot::poll`
    /// compares one enum — so an open panel over a settled file asks for no
    /// frames at all (PLAN §1).
    fn sync_spot(&mut self) -> bool {
        if self.spot.is_none() {
            return false;
        }
        let path = self.spot.as_ref().map(|s| s.facts.path.clone());
        // The card asks about one path, so unlike the rows it may start the
        // worker for a directory the list pane never needed one in: opening the
        // panel is an explicit question about *this* file.
        let git = path.as_deref().and_then(|path| {
            let git = self.git();
            git_line(git, path)
        });
        let Some(spot) = &mut self.spot else {
            return false;
        };
        let mut changed = spot.poll();
        if spot.facts.git != git {
            spot.facts.git = git;
            spot.refresh();
            changed = true;
        }
        changed
    }

    /// `Space` / `Enter` on the card's focused row.
    fn spot_action(&mut self, now: Instant) {
        let Some(action) = self.spot.as_ref().map(Spot::activate) else {
            return;
        };
        match action {
            spot::Action::SetMode(mode) => self.set_mode(mode, now),
            spot::Action::StartChecksum => {
                let waker = self.waker.named("checksum");
                if let Some(spot) = &mut self.spot {
                    spot.start_checksum(move || waker.wake());
                }
            }
            spot::Action::CancelChecksum => {
                if let Some(spot) = &mut self.spot {
                    spot.cancel_checksum();
                }
            }
            spot::Action::None => {}
        }
    }

    /// Write a new mode to the spotted file.
    ///
    /// **Not undoable, and the toast says so.** PLAN §5's journal has records
    /// for every operation that moves bytes around — copy, rename, trash,
    /// create, link — and none for a mode change, because df-core has no chmod
    /// operation at all. So this goes straight to `set_permissions` and raises a
    /// plain notice rather than the undo toast every other mutation gets: a
    /// toast that offered `u` and then did nothing would be worse than no offer.
    /// When df-core grows `ops::chmod` and an `OpRecord::Chmod`, this is the one
    /// call site that changes.
    fn set_mode(&mut self, mode: u32, now: Instant) {
        use std::os::unix::fs::PermissionsExt;
        let Some(facts) = self.spot.as_ref().map(|s| s.facts.clone()) else {
            return;
        };
        let path = facts.path;
        // A remote row's `path` is an `sftp://…` URL, which is a *relative*
        // `PathBuf` — `set_permissions` would resolve it against the process's
        // own directory. The vfs has a `chmod`; until this call site uses it,
        // the card's chips do not act remotely.
        if crate::remote::at_of(&path).is_some() {
            self.toasts
                .notice("Permissions cannot be changed over sftp yet", now);
            return;
        }
        // `chmod` follows a symlink, and there is no `lchmod` on Linux. So
        // applying these chips to a link row would change the mode of the file
        // it points at — a file the user did not select and which may not be in
        // this directory at all. The card already shows the *target's* mode,
        // which makes the swap invisible. Refusing and naming the target is the
        // only honest answer.
        if let Some(target) = &facts.link_target {
            self.toasts.error(
                format!(
                    "{} is a link to {}: changing these would change that file's permissions",
                    facts.name,
                    target.display()
                ),
                now,
            );
            return;
        }
        match std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)) {
            Ok(()) => {
                if let Some(spot) = &mut self.spot {
                    spot.facts.mode = mode;
                }
                self.toasts
                    .notice(format!("Permissions are now {}", spot::octal(mode)), now);
                // The list's permissions linemode is showing the old string
                // until the directory is read again.
                if let Some(dir) = path.parent().map(Path::to_path_buf) {
                    self.rescan(&dir, now);
                }
            }
            Err(e) => {
                // The common one is somebody else's file, and the message says
                // which file rather than only which errno.
                self.toasts.error(format!("{}: {e}", path.display()), now);
            }
        }
    }

    /// `w`: the task panel, which the same key closes again.
    fn toggle_panel(&mut self) {
        self.panel = match self.panel.take() {
            Some(_) => None,
            None => Some(TaskPanel::new()),
        };
        self.sync_context();
    }

    /// The `w` panel's rows, from the engine's snapshot — the render source its
    /// documentation insists on.
    fn task_rows(&self) -> Vec<TaskRow> {
        let mut snapshot = self.engine.snapshot();
        // Newest first: the thing you pressed `w` to look at is the thing that
        // just started.
        snapshot.sort_by_key(|task| std::cmp::Reverse(task.id));
        snapshot.iter().map(panel::row_for).collect()
    }

    fn cancel_selected_task(&mut self, now: Instant) {
        let rows = self.task_rows();
        let Some(id) = self.panel.as_ref().and_then(|p| p.selected(&rows)) else {
            return;
        };
        self.engine.cancel(id);
        self.toasts.notice("Cancelling…", now);
    }

    /// `p` in the panel. Pause and resume are one key, because a paused task's
    /// row already says which of the two pressing it will do.
    fn pause_selected_task(&mut self, now: Instant) {
        let rows = self.task_rows();
        let Some(row) = self
            .panel
            .as_ref()
            .and_then(|p| rows.get(p.cursor))
            .cloned()
        else {
            return;
        };
        if row.tone == panel::Tone::Paused {
            self.engine.resume(row.id);
            // The sticky toast the pause put up described a state that has now
            // ended, so it comes down with it (PLAN §5).
            self.toasts.clear_sticky(now);
        } else {
            self.engine.pause(row.id);
            self.toasts
                .sticky(format!("{} — paused, p resumes", row.name), now);
        }
    }

    // ── Prompts (PLAN §4.2) ─────────────────────────────────────────────────

    /// Open a prompt. `origin` is where the cursor is, which is what `Esc` puts
    /// back and what every keystroke of a live find searches from.
    fn open_prompt(&mut self, kind: PromptKind) {
        // Re-opening a filter edits the query that is applied rather than
        // starting from nothing — `f`, look, `f` again, refine. (Not the finds:
        // `/` is a new search, and the old one is on `n`.)
        let existing = match kind {
            PromptKind::Filter => self.tab().cwd.dir.filter().to_string(),
            PromptKind::HelpFilter => self.help_query.clone(),
            _ => String::new(),
        };
        let cursor = existing.chars().count();
        self.open_prompt_with(kind, InputBuffer::new(existing, cursor));
    }

    fn open_prompt_with(&mut self, kind: PromptKind, buffer: InputBuffer) {
        let origin = self.tab().cwd.dir.cursor();
        self.prompt = Some(Prompt::with(kind, origin, buffer));
        self.sync_context();
        self.prompt_changed();
    }

    /// `r` and `R`: the two rename presets, both anchored to the cursor's row.
    fn open_rename(&mut self, empty_stem: bool, now: Instant) {
        // PLAN §5: a selection of more than one is a *bulk* rename, and it gets
        // the diff card rather than a popup that would ask about the first file
        // and quietly ignore the rest. One selected file is still the popup —
        // that is what `r` has always meant, and a card for one name would be a
        // modal to change one word.
        if self.tab().cwd.dir.selected_count() > 1 {
            self.open_bulk(now);
            return;
        }
        let Some(entry) = self.tab().cwd.dir.cursor_entry() else {
            return;
        };
        let name = entry.name.clone();
        let (kind, buffer) = if empty_stem {
            (
                PromptKind::RenameEmptyStem,
                InputBuffer::for_rename_empty(&name),
            )
        } else {
            (PromptKind::Rename, InputBuffer::for_rename_stem(&name))
        };
        self.open_prompt_with(kind, buffer);
    }

    /// Open the two-column rename diff over the selection (PLAN §5).
    fn open_bulk(&mut self, now: Instant) {
        let dir = self.cwd();
        let listing = &self.tab().cwd.dir;
        // The selection in *listing* order, not in the order it was made: the
        // card is read against the pane behind it, and rows in a different
        // order from the pane would be a puzzle.
        let names: Vec<String> = listing
            .entries()
            .iter()
            .filter(|entry| listing.is_selected(&entry.name))
            .map(|entry| entry.name.clone())
            .collect();
        if names.len() < 2 {
            return;
        }
        // Every name in the directory, including the hidden ones and the ones
        // a filter is hiding: a name is taken whether or not you can see it.
        let siblings: Vec<String> = listing
            .entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        // A card that cannot be built is a card that would have renamed the
        // wrong thing — the constructor is the guard, and this is its sentence
        // (a remote pane: `r` on one row is a `RENAME` over the link, which
        // works, and a card of them is not implemented).
        match crate::bulk::Bulk::new(dir, names, &siblings) {
            Ok(card) => {
                self.dialog = Some(Dialog::Bulk(Box::new(card)));
                self.sync_context();
            }
            Err(why) => self.toasts.notice(why, now),
        }
    }

    /// One keystroke into the bulk-rename card.
    ///
    /// Returns whether it was taken. The card is a grid of line editors, so
    /// almost every key goes to whichever one has the caret — the same rule the
    /// bottom-bar prompt follows, and the reason a `q` typed into a name is a
    /// `q`. Only the keys the vi editor has no use for are intercepted:
    /// `Tab`/`Shift+Tab` and the two arrows, which move between fields.
    fn bulk_key(&mut self, chord: Chord, now: Instant) -> bool {
        let Some(Dialog::Bulk(bulk)) = &mut self.dialog else {
            return false;
        };
        let plain = chord.mods.is_none();
        match chord.key {
            Key::Tab if plain => {
                bulk.step(1);
                return true;
            }
            Key::Tab if chord.mods == df_core::keymap::Mods::SHIFT => {
                bulk.step(-1);
                return true;
            }
            Key::ArrowUp if plain => {
                bulk.step(-1);
                return true;
            }
            Key::ArrowDown if plain => {
                bulk.step(1);
                return true;
            }
            _ => {}
        }
        let field = bulk.field;
        let Some(buffer) = bulk.buffer_mut() else {
            return false;
        };
        match buffer.feed(chord) {
            InputEvent::Consumed => {
                match field {
                    // The two top fields rewrite every row that has not been
                    // hand-edited, live, as you type.
                    crate::bulk::Field::Find | crate::bulk::Field::Replace => bulk.apply_replace(),
                    // …and typing in a row is what exempts it from that.
                    crate::bulk::Field::Row(_) => bulk.touched(),
                }
                true
            }
            InputEvent::Submit(_) => {
                self.submit_bulk(now);
                true
            }
            InputEvent::Cancel => {
                self.close_overlay(now);
                true
            }
        }
    }

    /// `Enter` on a card with nothing wrong on it.
    fn submit_bulk(&mut self, now: Instant) {
        let Some(Dialog::Bulk(bulk)) = &self.dialog else {
            return;
        };
        if !bulk.valid() {
            // Refused rather than partly done: the card already says which rows
            // are wrong and in what way, so there is nothing to add.
            return;
        }
        if bulk.changes() == 0 {
            self.close_overlay(now);
            return;
        }
        let renames = bulk.renames();
        let dir = bulk.dir.clone();
        let cursor_on = bulk
            .rows
            .iter()
            .find(|row| row.changed())
            .map(|row| row.new_name().to_string());
        self.dialog = None;
        self.sync_context();
        self.run_bulk(renames, dir, cursor_on, now);
    }

    /// Carry the renames out, as one journal entry.
    ///
    /// On the calling thread, not on the pool: a rename is a `renameat2` and a
    /// card of forty of them is forty syscalls in the same directory — faster
    /// than the frame it was asked in. Sending it to a worker would buy nothing
    /// and cost the guarantee that the listing on screen after `Enter` is the
    /// listing the rename produced.
    ///
    /// Journalled as [`OpRecord::Renames`], which is one `u` for the whole card
    /// (PLAN §5). A failure part way records what did land, so `u` still takes
    /// back exactly what happened.
    fn run_bulk(
        &mut self,
        renames: Vec<(PathBuf, PathBuf)>,
        dir: PathBuf,
        cursor_on: Option<String>,
        now: Instant,
    ) {
        let mut moved: Vec<MovedPath> = Vec::new();
        let mut failed: Option<String> = None;
        for (from, to) in &renames {
            if let Err(e) = df_core::ops::create::rename(from, to, false) {
                failed = Some(e.to_string());
                break;
            }
            match MovedPath::record(from, to) {
                Ok(record) => moved.push(record),
                // The rename happened but cannot be described, so it cannot be
                // undone. Stop rather than build a record with a hole in it.
                Err(e) => {
                    failed = Some(e.to_string());
                    break;
                }
            }
        }
        // The temporary legs of a swap are internal: the record has to describe
        // where each file *started* and where it *ended*, or `u` would put a
        // file back to a name that was only ever a detour. Collapsing the chain
        // is what makes a swap undoable in one press.
        let moved = collapse_renames(moved);
        let count = moved.len();
        if !moved.is_empty() {
            self.journal.record(OpRecord::Renames { moved });
        }
        match failed {
            Some(error) => self.toasts.error(error, now),
            None => self
                .toasts
                .undo(format!("Renamed {}", plural(count, "file", "files")), now),
        }
        self.rescan(&dir, now);
        if let Some(name) = cursor_on {
            // The cursor follows the first file that moved, so the card closes
            // onto the change it made rather than onto wherever the list
            // happens to sort it (`delightful-ui` §8).
            self.tabs.active_mut().cwd.dir.cursor_to_name(&name);
        }
    }

    /// One keystroke into the open prompt.
    ///
    /// The buffer takes **every** key — that is what df-core's editor is for,
    /// and it is why a stray `q` in a rename types a `q` instead of quitting.
    fn prompt_key(&mut self, chord: Chord, now: Instant) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let live = prompt.kind.is_live();
        match prompt.feed(chord) {
            InputEvent::Consumed => {
                if live {
                    self.prompt_changed();
                }
            }
            InputEvent::Submit(text) => self.submit_prompt(text, now),
            InputEvent::Cancel => self.cancel_prompt(),
        }
    }

    /// Everything that has to happen when a live query changes:
    /// filter-as-you-type, find-as-you-type, and the help sheet narrowing under
    /// the cursor.
    fn prompt_changed(&mut self) {
        let Some(prompt) = &self.prompt else { return };
        let (kind, query, origin) = (prompt.kind, prompt.query().to_string(), prompt.origin);
        match kind {
            PromptKind::Filter => self.dir().set_filter(query),
            PromptKind::FindNext | PromptKind::FindPrev => {
                let direction = find_direction(kind);
                let dir = self.dir();
                // From the origin every time, not from the last match: typing a
                // second letter must narrow the search, never walk it forward
                // through the directory.
                dir.set_cursor(origin);
                if !query.is_empty() {
                    dir.find(&query, direction);
                }
            }
            PromptKind::HelpFilter => {
                self.help_query = query;
                // The line the cursor was on may not be in the sheet any more,
                // so it goes back to the first binding — which is also the one
                // the narrowed list is *about*.
                if let Some(mut help) = self.help {
                    let lines = self.help_lines();
                    help.reset(&lines);
                    self.help = Some(help);
                }
            }
            _ => {}
        }
        self.apply_visual();
    }

    /// `Enter`. A prompt whose work *failed* stays open with the reason beside
    /// it (PLAN §5: "errors as inline bar text, not toasts"), because the fix is
    /// almost always one more keystroke in the field you are already in.
    fn submit_prompt(&mut self, text: String, now: Instant) {
        let Some(kind) = self.prompt.as_ref().map(|p| p.kind) else {
            return;
        };
        let error = match kind {
            PromptKind::Filter | PromptKind::HelpFilter => None,
            PromptKind::FindNext | PromptKind::FindPrev => {
                // What `n` and `N` repeat.
                self.last_find = Some((text.clone(), find_direction(kind)));
                None
            }
            PromptKind::Create => self.create(&text, now).err(),
            PromptKind::Rename | PromptKind::RenameEmptyStem => self.rename(&text, now).err(),
            PromptKind::Shell => {
                let paths = self.targets();
                self.run_shell(&text, paths, false, now);
                None
            }
            PromptKind::ShellBlock => {
                let paths = self.targets();
                self.run_shell(&text, paths, true, now);
                None
            }
            PromptKind::ConflictRename => {
                self.conflict_rename(&text, now);
                // The dialog owns the outcome: it either takes the name or puts
                // its own message on the prompt it re-opened.
                return;
            }
        };
        match error {
            Some(message) => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.error = Some(message);
                }
            }
            None => {
                self.prompt = None;
                self.sync_context();
            }
        }
    }

    /// `Esc`: undo what the prompt did and close it.
    fn cancel_prompt(&mut self) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        match prompt.kind {
            PromptKind::Filter => self.dir().clear_filter(),
            PromptKind::FindNext | PromptKind::FindPrev => {
                let origin = prompt.origin;
                self.dir().set_cursor(origin);
            }
            PromptKind::HelpFilter => self.help_query.clear(),
            // A cancelled conflict rename goes back to the dialog, which is
            // still holding the unanswered conflict.
            _ => {}
        }
        self.sync_context();
    }

    /// `a`. A trailing `/` means a directory, and missing parents are made
    /// (df-core's `create`, which records what it had to make so `u` can peel
    /// them off again).
    fn create(&mut self, text: &str, now: Instant) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("no name given".to_string());
        }
        if let Some(at) = self.remote_at() {
            return self.remote_create(at, text);
        }
        let path = self.cwd().join(text);
        let created = df_core::ops::create(&path).map_err(|e| e.to_string())?;
        let name = created
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Ok(fingerprint) = Fingerprint::of(&created.path) {
            self.journal.record(OpRecord::Create {
                path: created.path.clone(),
                is_dir: created.is_dir,
                fingerprint,
                created_parents: created.created_parents,
            });
        }
        self.toasts.undo(
            format!(
                "Created {} {name}",
                if created.is_dir { "folder" } else { "file" }
            ),
            now,
        );
        let cwd = self.cwd();
        self.rescan(&cwd, now);
        self.dir().cursor_to_name(&name);
        Ok(())
    }

    /// `r` / `R`.
    fn rename(&mut self, text: &str, now: Instant) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("no name given".to_string());
        }
        if let Some(at) = self.remote_at() {
            return self.remote_rename(at, text);
        }
        let Some(from) = self
            .tab()
            .cwd
            .dir
            .cursor_entry()
            .map(|entry| entry.path.clone())
        else {
            return Err("Nothing under the cursor".to_string());
        };
        let to = self.cwd().join(text);
        df_core::ops::rename(&from, &to, false).map_err(|e| e.to_string())?;
        if let Ok(moved) = MovedPath::record(&from, &to) {
            self.journal.record(OpRecord::Rename { moved });
        }
        let name = to
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.toasts.undo(format!("Renamed to {name}"), now);
        let cwd = self.cwd();
        self.rescan(&cwd, now);
        self.dir().cursor_to_name(&name);
        Ok(())
    }

    /// The conflict dialog's rename came back from the prompt.
    fn conflict_rename(&mut self, text: &str, now: Instant) {
        let Some(Dialog::Conflict(dialog)) = &mut self.dialog else {
            self.prompt = None;
            self.sync_context();
            return;
        };
        match dialog.rename(text.trim()) {
            Step::NeedName(name) => {
                // Refused: put the message on the prompt and leave it open, with
                // what was typed still in it.
                if let Some(prompt) = &mut self.prompt {
                    prompt.error = dialog.error.clone();
                    let _ = name;
                }
            }
            Step::Continue => {
                self.prompt = None;
                self.sync_context();
            }
            Step::Settled => {
                let plan = match self.dialog.take() {
                    Some(Dialog::Conflict(dialog)) => Some(dialog.plan),
                    other => {
                        self.dialog = other;
                        None
                    }
                };
                self.prompt = None;
                if let Some(plan) = plan {
                    self.spawn_paste(plan, now);
                }
                self.sync_context();
            }
        }
    }

    /// `n` / `N` — repeat the last find, wrapping (PLAN §4.1).
    fn repeat_find(&mut self, reverse: bool) {
        let Some((query, direction)) = self.last_find.clone() else {
            log::debug!("nothing to repeat: no find has been made yet");
            return;
        };
        let direction = if reverse { flip(direction) } else { direction };
        if !self.dir().find(&query, direction) {
            log::debug!("`{query}` matches nothing here");
        }
        self.apply_visual();
    }

    // ── Selection (PLAN §4.1) ───────────────────────────────────────────────

    /// `Space`: toggle the row under the cursor, then advance — the advance is
    /// the binding's decision, not the model's, which is why it is here.
    fn toggle_select(&mut self) {
        let dir = self.dir();
        let at = dir.cursor();
        dir.toggle_selected(at);
        dir.move_cursor(1);
    }

    /// `v` and `V`. Pressing the same key again leaves the mode, keeping
    /// whatever the run selected — visual mode applies as it goes, so there is
    /// nothing left to commit.
    fn begin_visual(&mut self, selecting: bool) {
        if self.visual.is_some() {
            self.visual = None;
            return;
        }
        let anchor = self.tab().cwd.dir.cursor();
        self.visual = Some(Visual::new(selecting, anchor));
        // The anchor row is in the run from the moment the mode opens: `v` then
        // `Esc` with nothing selected would be a mode that did nothing.
        self.apply_visual();
    }

    /// Bring the selection in line with where the cursor has got to.
    ///
    /// Called after *every* command, because any of them can move the cursor
    /// and none of them should have to remember that visual mode is on. It is a
    /// no-op when it is not.
    fn apply_visual(&mut self) {
        let Some(visual) = self.visual.as_mut() else {
            return;
        };
        let dir = &mut self.tabs.active_mut().cwd.dir;
        if dir.is_empty() {
            return;
        }
        let last = dir.len() - 1;
        let wanted = select::range(visual.anchor.min(last), dir.cursor().min(last));
        let (leaving, entering) = select::range_delta(visual.applied, wanted);
        for position in leaving {
            let Some(name) = dir.row(position).map(|e| e.name.clone()) else {
                continue;
            };
            // Back to what the row was before visual mode reached it — which
            // for a row selected earlier with `Space` is *selected*.
            let was = visual.was_selected(&name).unwrap_or(false);
            dir.select_range(position, position, was);
        }
        for position in entering {
            let Some(name) = dir.row(position).map(|e| e.name.clone()) else {
                continue;
            };
            visual.remember(&name, dir.is_selected(&name));
            dir.select_range(position, position, visual.selecting);
        }
        visual.applied = Some(wanted);
    }

    // ── Overlays ────────────────────────────────────────────────────────────

    fn open_help(&mut self) {
        if self.help.is_some() {
            return;
        }
        self.help = Some(Help::default());
        self.help_query.clear();
        self.sync_context();
        // On the first binding rather than on the "Help" heading above it: the
        // cursor must always be on something you could act on.
        let lines = self.help_lines();
        if let Some(help) = &mut self.help {
            help.reset(&lines);
        }
    }

    fn close_help(&mut self) {
        self.help = None;
        self.help_query.clear();
        if self.prompt.as_ref().is_some_and(|p| p.kind.is_help()) {
            self.prompt = None;
        }
        self.sync_context();
    }

    /// The context stack, rebuilt from what is open (PLAN §4).
    ///
    /// The overlays *stack* rather than replace, which is the model df-core's
    /// registry is built on: Help sits on Files, and where both bind a key the
    /// more specific one wins — `f` filters the help sheet, `↑` walks it — while
    /// the browser's own keys stay reachable underneath. That is also what makes
    /// the sheet honest, because it lists exactly what this stack can dispatch.
    fn sync_context(&mut self) {
        // A modal surface is not *stacked* on the browser — see
        // [`App::route_keys`] — but the stack still has to say what is up, so
        // the hint bar and the help sheet describe the keyboard as it actually
        // is.
        if self.overlay_open() {
            self.context = self.overlay_stack();
            if self.prompt.is_some() {
                self.context.push(Context::Input);
            }
            return;
        }
        let mut stack = self.help_stack();
        if self.prompt.is_some() {
            stack.push(Context::Input);
        }
        self.context = stack;
    }

    /// The stack the help sheet documents: the browser, plus the help overlay
    /// when it is open. Deliberately *without* the Input context that an open
    /// prompt adds — a filter box that rewrote the list it was filtering into a
    /// list of its own bindings would be no use to anybody.
    fn help_stack(&self) -> ContextStack {
        let mut stack = ContextStack::browser();
        if self.help.is_some() {
            stack.push(Context::Help);
        }
        stack
    }

    /// The `Esc` ladder (PLAN §4.1), one rung per press: cancel the chord →
    /// close the prompt → close the overlay → leave visual mode → clear the
    /// selection → clear the filter.
    ///
    /// One rung at a time is the whole point. `Esc` that cleared everything
    /// would mean a stray press throws away a selection built up over a dozen
    /// keystrokes, and there is no undo for a selection.
    fn escape(&mut self) {
        // Two rungs above the ladder proper, both owned by the pointer, and
        // both above everything else for the same reason: they are things the
        // hand is doing *now*. The menu is handled in `menu_key` before a
        // chord is ever dispatched; a band is not, because a drag does not
        // take the keyboard.
        if self.band.is_some() {
            self.cancel_band();
            return;
        }
        // The third, and above both: a drag is the most immediate thing the
        // hand is doing, and `Esc` cancelling anything else while files are
        // being carried would be `Esc` answering a question nobody asked.
        // A drag the compositor has taken is *not* cancellable from here — the
        // pointer is not ours, and only the compositor can call that off.
        if self.drag.as_ref().is_some_and(|drag| !drag.handed_off) {
            if let Some(drag) = self.drag.take() {
                self.spring_home(drag, Instant::now());
            }
            self.press = None;
            return;
        }
        // A tab in the hand is the same kind of thing and gets the same rung
        // (PLAN §2): `Esc` puts the chip back, and nothing is opened or closed.
        if let Some(drag) = self.tab_drag.take() {
            let at = drag.at;
            self.spring_tab_home(drag, at, Instant::now());
            self.press = None;
            return;
        }
        // The tray is the smallest thing on screen that `Esc` can take back,
        // so it goes first: closing it is never what somebody meant `Esc` to
        // do *instead* of something bigger.
        if self.basket_open {
            self.basket_open = false;
            return;
        }
        // Above the ladder proper for the same reason a band is: the usage
        // mode is a state the user turned on a moment ago and expects `Esc` to
        // turn off, and it is a *bigger* change to the pane than clearing a
        // selection is (PLAN §7.3: "Esc/toggle leaves").
        if self.usage.is_some() {
            self.leave_usage(Instant::now());
            return;
        }
        // The rungs and their order are [`crate::focus::escape_rung`]'s, so the
        // ladder can be walked in a test with no window and no dialog; this is
        // only the doing of them.
        let state = EscapeState {
            // A modal card is the nearest thing to the user, and its own `Esc`
            // knows whether it is holding a prompt of its own.
            overlay_open: self.overlay_open(),
            chord_pending: self.keys.is_pending(),
            prompt_open: self.prompt.is_some(),
            help_open: self.help.is_some(),
            visual: self.visual.is_some(),
            selection: self.dir().selected_count() > 0,
            filter: !self.dir().filter().is_empty(),
            focus: self.focus,
        };
        match escape_rung(state) {
            EscapeRung::CloseOverlay => self.close_overlay(Instant::now()),
            EscapeRung::CancelChord => self.keys.cancel(),
            EscapeRung::ClosePrompt => self.cancel_prompt(),
            EscapeRung::CloseHelp => self.close_help(),
            EscapeRung::LeaveVisual => {
                self.visual = None;
            }
            EscapeRung::ClearSelection => self.dir().clear_selection(),
            EscapeRung::ClearFilter => self.dir().clear_filter(),
            // The last rung, and the one PLAN §4.1 ends on: whatever else is
            // going on, `Esc` gets you back to the list.
            EscapeRung::FocusList => self.focus = Focus::List,
            EscapeRung::Nothing => {}
        }
    }

    // ── Tabs (PLAN §2) ──────────────────────────────────────────────────────

    /// Anything that changes which tab is on screen: the watcher follows, a
    /// visual run anchored in the other tab ends, and the newly shown
    /// directories are re-read — they were not being watched while they were
    /// out of sight.
    fn tab_changed(&mut self, now: Instant) {
        self.visual = None;
        // The other tab's cursor is on a different file, so whatever the
        // preview is decoding is work for a pane nobody is looking at. The
        // next frame's `sync` asks for the new tab's file.
        self.preview.cancel();
        // A tab switch is a deliberate move to somewhere else, not a flick past
        // a row: the source goes now rather than after the grace, because the
        // file it was holding belongs to a directory that is no longer on
        // screen.
        self.close_player();
        self.tabs.active_mut().rescan(&self.scanner, now);
        self.rewatch();
    }

    /// The three gates a verb passes before it acts, in one place.
    ///
    /// Returns whether the command was refused (and said so). Where the list
    /// pane is showing something that is not a directory on this machine —
    /// an archive's interior, a remote service, the trash — a whole class of
    /// verbs would act on a path that cannot be acted on: an `sftp://…` display
    /// path is a *relative* `PathBuf`, and handing one to `rename(2)`, to a
    /// child process or to the trash would write somewhere nobody was looking.
    ///
    /// **Every door to a verb goes through here**, not just the keyboard: the
    /// context menu is a second dispatch over the same verbs (see
    /// [`menu_command`]), and it used to reach them with only a hand-written
    /// branch for `d` standing between a remote row and a local trash job.
    fn refuse_where_we_are(&mut self, command: Command, now: Instant) -> bool {
        // PLAN §7.3: an archive browsed as a directory is read-only in v1, and
        // the commands that would write into one are inert *out loud*. A key
        // that silently does nothing is a key the user presses twice — and the
        // notice names the way out, which is the whole point of saying it.
        if self.tab().archive.is_some() && crate::archive::inert_in_archive(command) {
            self.toasts.notice(
                "Archives are read-only — press e to extract".to_string(),
                now,
            );
            return true;
        }
        // PLAN §7.6: the same rule for a remote service, with a different list
        // and a different sentence. What is missing is missing for a reason the
        // notice names, so the key is not simply dead.
        if self.tab().remote.is_some() && crate::remote::inert_remotely(command) {
            self.toasts.notice(
                "Not over the link — press y then p in a local folder to bring it here".to_string(),
                now,
            );
            return true;
        }
        // PLAN §7.4: the trash is a listing of things that have already been
        // deleted. Everything that would act on them *as files where they are*
        // is inert, and the three verbs that are not are Enter/r, D and the
        // palette's "Empty trash".
        if self.tab().trash.is_some() && inert_in_trash(command) {
            self.toasts.notice(
                "Not in the trash — Enter restores, D destroys".to_string(),
                now,
            );
            return true;
        }
        false
    }

    fn run(&mut self, command: Command, page: usize, now: Instant) {
        use Command as C;
        // A command that shuffles the rows you are looking at gets a FLIP
        // (PLAN §2, §8): where everything is *now* is captured before the
        // command runs, because afterwards it is gone. The next frame's layout
        // is the other half.
        if flip::reorders(command) {
            self.flip_before = Some(self.last_layout.clone());
        }
        if self.refuse_where_we_are(command, now) {
            return;
        }
        // Half a page rounds *down* but never to nothing: on a pane too short
        // to have a half, `Ctrl+d` still has to move.
        let half = (page / 2).max(1) as isize;
        let full = page.max(1) as isize;

        match command {
            // ── The cursor ──────────────────────────────────────────────────
            C::CursorUp => {
                self.step_cursor(grid::Step::Up);
            }
            C::CursorDown => {
                self.step_cursor(grid::Step::Down);
            }
            // A page is a page of the *pane*, so in a grid it is that many
            // rows of tiles rather than that many files.
            C::HalfPageUp => self.page_cursor(-half),
            C::HalfPageDown => self.page_cursor(half),
            C::PageUp => self.page_cursor(-full),
            C::PageDown => self.page_cursor(full),
            C::CursorTop => self.dir().set_cursor(0),
            C::CursorBottom => self.dir().set_cursor(usize::MAX),

            // ── Moving between directories ──────────────────────────────────
            C::Leave => {
                // In a grid, `←` is "the previous tile" — the tiles are one
                // sequence read like a page (PLAN §2, `grid::step`). It only
                // means "leave" at the very first tile, where there is no
                // previous one and the key would otherwise do nothing at all,
                // so no capability is lost and no key changes meaning anywhere
                // it had one.
                if self.columns > 1 && self.step_cursor(grid::Step::Left) {
                    self.apply_visual();
                    return;
                }
                // `←` on a remote service walks up the remote tree, and at the
                // service root walks *out* — back to the local directory the
                // session started in (`crate::remote::leave`).
                if let Some(at) = self.remote_at() {
                    let origin = self.local_origin();
                    match crate::remote::leave(&at, &origin) {
                        crate::remote::Leave::Up(up) => self.enter_remote(up, now),
                        crate::remote::Leave::Out(home) => self.navigate(home, now),
                    }
                    return;
                }
                // The trash has no inside, so `←` is the only way out of it.
                if let Some(origin) = self.tab().trash.as_ref().map(|v| v.origin.clone()) {
                    self.navigate(origin, now);
                    return;
                }
                if let Some(parent) = self.tab().cwd.path().parent().map(Path::to_path_buf) {
                    self.navigate(parent, now);
                }
            }
            C::EnterOrPreview => {
                // The mirror of `Leave` above: in a grid `→` is the next tile,
                // and only the last tile enters or focuses the preview.
                if self.columns > 1 && self.step_cursor(grid::Step::Right) {
                    self.apply_visual();
                    return;
                }
                // PLAN §2.1's "rightward": a directory is a place and `→` goes
                // there; a file has no inside, so `→` goes to the pane that is
                // already showing it.
                let hovered = match self.tab().cwd.dir.cursor_entry() {
                    // In the trash even a folder is a *thing*, not a place: its
                    // real path is inside `…/Trash/files/`, and walking in
                    // would leave the view and strand somebody two levels down
                    // a directory they never chose to open (PLAN §7.4). `→`
                    // focuses the preview, which is what the row is for.
                    Some(_) if self.tab().trash.is_some() => Hovered::File,
                    Some(entry) if entry.is_dir() => Hovered::Directory,
                    Some(_) => Hovered::File,
                    None => Hovered::Nothing,
                };
                match crate::focus::rightward(hovered) {
                    Rightward::Enter => {
                        if let Some(entry) = self.tab().cwd.dir.cursor_entry() {
                            let path = entry.path.clone();
                            self.navigate(path, now);
                        }
                    }
                    Rightward::FocusPreview => {
                        // PLAN §7.3: an archive *is* a place, so `→` on one goes
                        // there rather than to the preview pane. Only in a real
                        // directory — an archive inside an archive is a file
                        // that has to come out first.
                        let archive = self
                            .tab()
                            .cwd
                            .dir
                            .cursor_entry()
                            .filter(|_| self.tab().archive.is_none())
                            // …and not on a remote service: the row's path is a
                            // URL, and the archive reader takes a file on this
                            // machine. `o` downloads it; `→` focuses the card.
                            .filter(|_| self.tab().remote.is_none())
                            .filter(|entry| crate::archive::looks_like_archive(entry))
                            .map(|entry| entry.path.clone());
                        match archive {
                            Some(path) => self.ask_archive(path, ArchiveIntent::Browse),
                            None => self.focus = Focus::Preview,
                        }
                    }
                    Rightward::Nothing => {}
                }
            }

            // ── The parent pane, once a click or `←` has put the keyboard in
            // it (PLAN §2.1's `in_parent`) ──────────────────────────────────
            C::ParentPrev => {
                if let Some(parent) = &mut self.tabs.active_mut().parent {
                    parent.dir.move_cursor(-1);
                }
            }
            C::ParentNext => {
                if let Some(parent) = &mut self.tabs.active_mut().parent {
                    parent.dir.move_cursor(1);
                }
            }
            C::ParentEnter => {
                // Entering from the parent pane puts the keyboard back in the
                // list: the pane you steered with has become the pane you came
                // from, and leaving focus behind would strand the cursor.
                let target = self
                    .tab()
                    .parent
                    .as_ref()
                    .and_then(|parent| parent.dir.cursor_entry())
                    .filter(|entry| entry.is_dir())
                    .map(|entry| entry.path.clone());
                if let Some(path) = target {
                    self.focus = Focus::List;
                    self.navigate(path, now);
                }
            }
            C::HistoryBack => {
                let (mgr, sort) = (self.mgr.clone(), self.sort());
                if self.tabs.active_mut().back(&mgr, sort, &self.scanner, now) {
                    self.visual = None;
                    self.rewatch();
                }
            }
            C::HistoryForward => {
                let (mgr, sort) = (self.mgr.clone(), self.sort());
                if self
                    .tabs
                    .active_mut()
                    .forward(&mgr, sort, &self.scanner, now)
                {
                    self.visual = None;
                    self.rewatch();
                }
            }
            C::Goto(slot) => self.goto(slot, now),
            C::OpenTrash => self.open_trash(now),
            C::EmptyTrash => self.open_confirm(ConfirmKind::EmptyTrash, now),

            // ── The preview, from the list (PLAN §4.1's yazi parity) ────────
            // **One key, two units.** yazi's `K`/`J` "seek preview ±5" moves a
            // document by five *lines*; on a mounted clip the same key moves it
            // by five *seconds* (`playback::SEEK_US`), because five lines of a
            // video is not a quantity. The split is on what is actually in the
            // pane, so nothing has to be un-learned in either direction.
            C::SeekPreviewUp | C::SeekPreviewDown => {
                let down = command == C::SeekPreviewDown;
                match crate::playback::list_seek(down, self.transport().is_some()) {
                    crate::playback::ListSeek::Micros(delta) => {
                        if let Some(player) = self.transport() {
                            player.skip(delta, now);
                        }
                    }
                    crate::playback::ListSeek::Lines(_) => self.preview.seek(down, now),
                }
            }

            // ── The preview, with the keyboard in it (PLAN §4.3) ────────────
            // **One key, two units, again.** In a text body `↑`/`↓` move a
            // line; in a rendered document they move the page under the pane,
            // and in a G-code toolpath they step a layer — which is the one
            // thing anybody opens a G-code file to do. `doc_scroll` answers
            // whether it took the key, so nothing has to ask what is on screen.
            C::PreviewUp => {
                if !self.preview.doc_scroll(-1, now) {
                    self.preview.scroll_by(-1, now);
                }
            }
            C::PreviewDown => {
                if !self.preview.doc_scroll(1, now) {
                    self.preview.scroll_by(1, now);
                }
            }
            C::PreviewHalfPageUp => {
                if !self.preview.doc_scroll(-(half.max(1)), now) {
                    self.preview.scroll_by(-half, now);
                }
            }
            C::PreviewHalfPageDown => {
                if !self.preview.doc_scroll(half.max(1), now) {
                    self.preview.scroll_by(half, now);
                }
            }
            C::PreviewPageDown => {
                if !self.preview.doc_scroll(full.max(1), now) {
                    self.preview.scroll_by(full, now);
                }
            }
            // `→` turns the page. `←` turns it back — and **at the first page
            // it falls through to the list** (PLAN §4.3), which is also what a
            // body with no pages at all does, so one key means "back" wherever
            // you are and never leaves you stuck in the pane.
            C::PreviewRight => {
                self.preview.turn_page(true, now);
            }
            C::PreviewLeft => {
                if !self.preview.turn_page(false, now) {
                    self.focus = Focus::List;
                }
            }
            C::PreviewZoomIn => {
                self.preview.zoom(crate::preview::Zoom::In, now);
            }
            C::PreviewZoomOut => {
                self.preview.zoom(crate::preview::Zoom::Out, now);
            }
            C::PreviewZoomReset => {
                self.preview.zoom(crate::preview::Zoom::Fit, now);
            }
            C::PreviewTop => self.preview.scroll_to(0, now),
            C::PreviewBottom => self.preview.scroll_to(usize::MAX, now),

            // ── Transport, on the hovered file, at any focus (PLAN §4.3) ────
            C::PlayPause => {
                if let Some(player) = self.transport() {
                    player.play_pause(now);
                }
            }
            C::ShuttleForward | C::ShuttleReverse => {
                let dir = if command == C::ShuttleForward {
                    1.0
                } else {
                    -1.0
                };
                let held = self.key_repeat;
                if let Some(player) = self.transport() {
                    player.shuttle(dir, held, now);
                }
            }
            C::ToggleLoop => {
                if let Some(player) = self.transport() {
                    let on = player.toggle_loop(now);
                    log::debug!("loop {}", if on { "on" } else { "off" });
                }
            }
            C::PrevEdge | C::NextEdge => {
                let end = command == C::NextEdge;
                if let Some(player) = self.transport() {
                    player.seek_edge(end, now);
                }
            }
            C::SkipBack | C::SkipForward => {
                let delta = if command == C::SkipForward {
                    crate::playback::SKIP_US
                } else {
                    -crate::playback::SKIP_US
                };
                if let Some(player) = self.transport() {
                    player.skip(delta, now);
                }
            }
            C::FrameStepBack | C::FrameStepForward => {
                let frames = if command == C::FrameStepForward {
                    1
                } else {
                    -1
                };
                if let Some(player) = self.transport() {
                    player.step(frames, now);
                }
            }
            C::Mute => {
                if let Some(player) = self.transport() {
                    player.toggle_mute(now);
                }
            }
            C::VolumeUp | C::VolumeDown => {
                let delta = if command == C::VolumeUp {
                    crate::playback::VOLUME_STEP
                } else {
                    -crate::playback::VOLUME_STEP
                };
                if let Some(player) = self.transport() {
                    player.nudge_volume(delta, now);
                }
            }

            // ── Selection ───────────────────────────────────────────────────
            C::ToggleSelect => self.toggle_select(),
            C::SelectAll => self.dir().select_all(),
            C::InvertSelection => self.dir().invert_selection(),
            C::VisualMode => self.begin_visual(true),
            C::VisualUnset => self.begin_visual(false),

            // ── Filter and find ─────────────────────────────────────────────
            C::Filter => self.open_prompt(PromptKind::Filter),
            C::FindNext => self.open_prompt(PromptKind::FindNext),
            C::FindPrev => self.open_prompt(PromptKind::FindPrev),
            C::FindArrowNext => self.repeat_find(false),
            C::FindArrowPrev => self.repeat_find(true),
            C::CancelSearch => {
                self.dir().clear_filter();
                self.last_find = None;
                // …and stops an `fd`/`rg` still walking, when one is (PLAN
                // §7.2). Reachable from the browser as well as from inside the
                // panel, because a search left running behind a closed panel is
                // exactly what this key is for.
                if let Some(search) = &mut self.search {
                    search.cancel();
                }
            }

            // ── Windows (PLAN §2) ───────────────────────────────────────────
            // `Ctrl+N`. The directory is the *local* one, for the same reason
            // the cwd-file's is: a window opened from inside an archive, the
            // trash or a remote listing has to start somewhere a second process
            // can actually read.
            C::NewWindow => {
                let dir = self.local_origin();
                self.open_window(&dir, now);
            }

            // ── Tabs ────────────────────────────────────────────────────────
            C::TabCreate => {
                let path = self.tab().cwd.path().to_path_buf();
                let (mgr, sort) = (self.mgr.clone(), self.sort());
                if self.tabs.create(path, &mgr, sort, &self.scanner, now) {
                    // No re-read: the tab was built a line ago and its scan is
                    // already in flight. Only the watcher has to follow.
                    self.visual = None;
                    self.rewatch();
                } else {
                    log::info!(
                        "{} tabs is the maximum — `1`–`9` is the switch row",
                        crate::tabs::MAX_TABS
                    );
                }
            }
            C::TabSwitch(n) => {
                if self.tabs.switch_to(n as usize, now) {
                    self.tab_changed(now);
                }
            }
            C::TabPrev => {
                if self.tabs.cycle(-1, now) {
                    self.tab_changed(now);
                }
            }
            C::TabNext => {
                if self.tabs.cycle(1, now) {
                    self.tab_changed(now);
                }
            }
            // Swapping does not change *which* tab you are looking at, so
            // nothing is re-read and nothing is re-watched: only the strip
            // moves.
            C::TabSwapPrev => {
                self.tabs.swap(-1);
            }
            C::TabSwapNext => {
                self.tabs.swap(1);
            }
            C::CloseTab => {
                if self.tabs.close_active() {
                    self.tab_changed(now);
                } else {
                    // The last tab: `Ctrl+c` is a quit, and a quit writes the
                    // cwd-file (PLAN §4.1).
                    self.quit = Some(Quit::WriteCwd);
                }
            }

            // ── Overlays ────────────────────────────────────────────────────
            // `~` and `F1` toggle: the key that opened the sheet is the one a
            // hand reaches for to get rid of it again.
            C::Help => {
                if self.help.is_some() {
                    self.close_help();
                } else {
                    self.open_help();
                }
            }
            C::HelpFilter => self.open_prompt(PromptKind::HelpFilter),
            C::OverlayClose => self.close_help(),
            C::OverlaySubmit => {}

            C::OverlayPrev | C::OverlayNext => {
                // The help browser is the only overlay with a list in it so
                // far; the others arrive with their phases.
                let delta = if command == C::OverlayPrev { -1 } else { 1 };
                self.move_help_cursor(delta);
            }

            // ── What is shown ───────────────────────────────────────────────
            C::ToggleHidden => {
                self.mgr.show_hidden = !self.mgr.show_hidden;
                let show = self.mgr.show_hidden;
                let tab = self.tabs.active_mut();
                tab.cwd.dir.set_show_hidden(show);
                if let Some(parent) = &mut tab.parent {
                    parent.dir.set_show_hidden(show);
                }
                tab.sync_parent_cursor();
            }
            C::LinemodeSize => self.mgr.linemode = LineMode::Size,
            C::LinemodePermissions => self.mgr.linemode = LineMode::Permissions,
            C::LinemodeBtime => self.mgr.linemode = LineMode::Btime,
            C::LinemodeMtime => self.mgr.linemode = LineMode::Mtime,
            C::LinemodeOwner => self.mgr.linemode = LineMode::Owner,
            C::LinemodeNone => self.mgr.linemode = LineMode::None,

            // ── Sort ────────────────────────────────────────────────────────
            C::SortMtime => self.sort_by(SortBy::Mtime, false, Some(LineMode::Mtime)),
            C::SortMtimeReverse => self.sort_by(SortBy::Mtime, true, Some(LineMode::Mtime)),
            C::SortBtime => self.sort_by(SortBy::Btime, false, Some(LineMode::Btime)),
            C::SortBtimeReverse => self.sort_by(SortBy::Btime, true, Some(LineMode::Btime)),
            C::DiskUsage => self.toggle_usage(now),
            C::MountManager => self.open_mounts(),
            C::BasketToggle => self.toss_basket(now),
            C::BasketShow => self.show_basket(now),
            C::SortSize => self.sort_by(SortBy::Size, false, Some(LineMode::Size)),
            C::SortSizeReverse => self.sort_by(SortBy::Size, true, Some(LineMode::Size)),
            C::SortExtension => self.sort_by(SortBy::Extension, false, None),
            C::SortExtensionReverse => self.sort_by(SortBy::Extension, true, None),
            C::SortAlphabetical => self.sort_by(SortBy::Alphabetical, false, None),
            C::SortAlphabeticalReverse => self.sort_by(SortBy::Alphabetical, true, None),
            C::SortNatural => self.sort_by(SortBy::Natural, false, None),
            C::SortNaturalReverse => self.sort_by(SortBy::Natural, true, None),
            C::SortRandom => {
                self.seed = random_seed();
                self.sort_by(SortBy::Random, false, None);
            }

            // ── The clipboard and the file operations (PLAN §5) ─────────────
            C::Yank => self.set_clipboard(false, now),
            C::YankCut => self.set_clipboard(true, now),
            C::Unyank => self.unyank(now),
            C::Paste => self.paste(false, now),
            C::PasteForce => self.paste(true, now),
            // ── The *system* clipboard (PLAN §7.4) ──────────────────────────
            C::CopyToClipboard => self.yank_to_system(now),
            C::CopyFileText => self.copy_file_text(now),
            C::CopyPath => self.copy_piece(Piece::Path, now),
            C::CopyDirname => self.copy_piece(Piece::Dirname, now),
            C::CopyFilename => self.copy_piece(Piece::Filename, now),
            C::CopyStem => self.copy_piece(Piece::Stem, now),

            // `d`: the trash locally, and a confirm that says there is no trash
            // remotely (PLAN §7.6). In the trash view it is on `inert_in_trash`,
            // because what it names has already been trashed.
            C::Trash => {
                let kind = if self.tab().remote.is_some() {
                    ConfirmKind::RemoteDelete
                } else {
                    ConfirmKind::Trash
                };
                self.open_confirm(kind, now);
            }
            C::DeletePermanently => {
                let kind = if self.tab().trash.is_some() {
                    ConfirmKind::Purge
                } else {
                    ConfirmKind::Delete
                };
                self.open_confirm(kind, now);
            }
            C::SymlinkAbsolute => self.link(Some(LinkKind::Absolute), now),
            C::SymlinkRelative => self.link(Some(LinkKind::Relative), now),
            C::Hardlink => self.link(None, now),
            C::Create => self.open_prompt(PromptKind::Create),
            // `r` in the trash is the same verb as `Enter`: a restore *is* the
            // move back to the name it had, which is the closest thing to a
            // rename the view has.
            C::Rename if self.tab().trash.is_some() => self.trash_restore(now),
            C::Rename => self.open_rename(false, now),
            C::RenameEmptyStem => self.open_rename(true, now),
            C::Shell => self.open_prompt(PromptKind::Shell),
            C::ShellBlock => self.open_prompt(PromptKind::ShellBlock),
            C::Undo => self.undo(now),
            C::TasksShow => self.toggle_panel(),
            C::ToggleView => self.toggle_view(now),
            C::Spot => self.toggle_spot(),

            // ── Opening (PLAN §6) ───────────────────────────────────────────
            C::Open => {
                // Inside an archive `Enter` has no opener to reach for — the
                // file is not on the disk. On a directory it walks in, which is
                // what `Enter` on a folder already means; on anything else it
                // extracts what is selected (PLAN §7.3).
                if self.tab().archive.is_some() {
                    let into = self
                        .tab()
                        .cwd
                        .dir
                        .cursor_entry()
                        .filter(|entry| entry.is_dir())
                        .filter(|_| self.tab().cwd.dir.selected_count() == 0)
                        .map(|entry| entry.path.clone());
                    match into {
                        Some(path) => self.navigate(path, now),
                        None => self.extract_selection(false, now),
                    }
                    return;
                }
                // PLAN §7.4: `Enter` in the trash is *restore*. It is the verb
                // somebody came here for, and it is one key from `u`'s meaning
                // — putting something back where it was.
                if self.tab().trash.is_some() {
                    self.trash_restore(now);
                    return;
                }
                self.open_hovered(now)
            }
            C::ArchiveExtractHere => self.extract(false, now),
            C::ArchiveExtractSubfolder => self.extract(true, now),
            C::OpenInteractive => self.open_picker(now),

            // ── The palette and the jumps (PLAN §4.4, §7.2) ─────────────────
            C::CommandPalette => self.open_palette(),
            C::FuzzyJump => self.open_jump(Source::Jump),
            C::ZoxideJump => self.open_jump(Source::Zoxide),
            C::SearchName => self.open_search(search::Mode::Names),
            C::SearchContent => self.open_search(search::Mode::Content),

            // ── Leaving ─────────────────────────────────────────────────────
            C::Quit => self.quit = Some(Quit::WriteCwd),
            C::QuitNoCwdFile => self.quit = Some(Quit::Silent),
            C::Escape => self.escape(),

            other => log::debug!("`{}` is not implemented yet", other.id()),
        }
        // Any of the above can move the cursor, and none of them should have to
        // remember that visual mode is on.
        self.apply_visual();
    }

    /// One arrow key's worth of cursor movement, in whichever geometry the
    /// pane is drawn in.
    ///
    /// Returns whether the cursor actually moved, which is what lets `←` and
    /// `→` fall through to "leave" and "enter" at the two edges where they have
    /// nowhere to go.
    fn step_cursor(&mut self, step: grid::Step) -> bool {
        let columns = self.columns;
        let dir = self.dir();
        let (before, count) = (dir.cursor(), dir.len());
        let after = grid::step(before, count, columns, step);
        if after == before {
            return false;
        }
        dir.set_cursor(after);
        true
    }

    /// A page of the pane, up or down — a page of *rows*, which in a grid is
    /// that many rows of tiles.
    fn page_cursor(&mut self, pages: isize) {
        let columns = self.columns.max(1) as isize;
        self.dir().move_cursor(pages * columns);
    }

    /// Move the help browser's cursor, if it is open.
    ///
    /// The lines are rebuilt here rather than cached because they are cheap and
    /// because the alternative is a cache that has to be invalidated by the
    /// filter, the context stack and every `keymap.toml` reload — three chances
    /// for the help sheet to disagree with the keymap, which is the one thing it
    /// exists not to do.
    fn move_help_cursor(&mut self, delta: isize) {
        let Some(mut help) = self.help else { return };
        let lines = self.help_lines();
        help.move_cursor(&lines, delta);
        self.help = Some(help);
    }

    /// The help sheet as it stands: every live binding, narrowed by `f`.
    fn help_lines(&self) -> Vec<crate::help::HelpLine> {
        let rows = help::all_rows(&self.keymap, &self.help_stack(), WhenFlags::LIST);
        help::lines(&rows, &self.help_query)
    }

    /// The `,` chord. PLAN §4.1: the time and size sorts **also switch the
    /// linemode**, as the yazi config this is ported from does — the column you
    /// just sorted by is the column you want to be able to see.
    fn sort_by(&mut self, by: SortBy, reverse: bool, linemode: Option<LineMode>) {
        self.mgr.sort_by = by;
        self.mgr.sort_reverse = reverse;
        if let Some(mode) = linemode {
            self.mgr.linemode = mode;
        }
        let sort = self.sort();
        // A directory peek is listed the way entering it would list it, so the
        // preview follows `,` too.
        self.preview.set_sort(sort);
        let tab = self.tabs.active_mut();
        tab.cwd.dir.set_sort(sort);
        if let Some(parent) = &mut tab.parent {
            parent.dir.set_sort(sort);
        }
        tab.sync_parent_cursor();
    }

    // ── "What's big" mode (PLAN §7.3) ───────────────────────────────────────

    /// The recursive-size walker, started the first time it is asked for.
    fn du(&mut self) -> &df_core::du::DuScanner {
        let waker = self.waker.named("du");
        self.du
            .get_or_insert_with(|| df_core::du::DuScanner::start(Arc::new(move || waker.wake())))
    }

    /// `m u`, and the palette's "Show disk usage": the mode goes on, or off.
    fn toggle_usage(&mut self, now: Instant) {
        if self.usage.is_some() {
            self.leave_usage(now);
            return;
        }
        let dir = self.cwd();
        // Depth 1: the mode shows this directory's children, and every level
        // below that is counted into them rather than listed.
        let token = self.du().request(dir.clone(), 1);
        let previous = SortOptions {
            by: self.mgr.sort_by,
            reverse: self.mgr.sort_reverse,
            ..self.sort()
        };
        self.usage = Some(crate::usage::Usage::new(dir, token, previous, now));
        // Biggest first — which *is* the drill-down (PLAN §7.3). The sort is
        // the existing one, applied by the existing command, so the FLIP
        // animation carries the rows to their new places exactly as `, S`
        // would.
        self.flip_before = Some(self.last_layout.clone());
        self.sort_by(SortBy::Size, true, None);
        self.toasts.notice("Measuring…", now);
    }

    /// `Esc`, the toggle again, or navigating away.
    fn leave_usage(&mut self, now: Instant) {
        let Some(usage) = self.usage.take() else {
            return;
        };
        if let Some(du) = &self.du {
            // A walk nobody is looking at is work nobody asked for (PLAN §1).
            du.cancel(usage.token);
        }
        self.flip_before = Some(self.last_layout.clone());
        self.sort_by(usage.previous_sort.by, usage.previous_sort.reverse, None);
        // Re-read, so the directory rows go back to the honest zero they
        // started as rather than keeping numbers nothing is maintaining.
        self.rescan(&usage.dir.clone(), now);
    }

    /// Take whatever the walk has said since the last frame.
    ///
    /// Returns whether anything on screen moved. Early-returns on the common
    /// case — the mode is off — so a session that never used it never touches
    /// a channel.
    fn poll_usage(&mut self, now: Instant) -> bool {
        if self.usage.is_none() {
            return false;
        }
        // The mode belongs to a directory; walking out of it ends it.
        let cwd = self.cwd();
        if self.usage.as_ref().is_some_and(|u| !u.is_about(&cwd)) {
            self.leave_usage(now);
            return true;
        }
        let messages = match &self.du {
            Some(du) => du.drain(),
            None => return false,
        };
        let mut changed = false;
        for message in messages {
            let Some(usage) = &mut self.usage else { break };
            if message.token() != usage.token {
                // A walk that has been superseded. Its numbers are about a
                // directory nobody is looking at.
                continue;
            }
            match message {
                df_core::du::DuMessage::Progress { updates, .. } => {
                    changed |= usage.apply(&updates);
                }
                df_core::du::DuMessage::Done { totals, .. } => {
                    usage.finish(totals.total_bytes);
                    let summary = format!(
                        "{} in {}",
                        crate::format::human_size(usage.total()),
                        plural(totals.files as usize, "file", "files")
                    );
                    self.toasts.notice(summary, now);
                    changed = true;
                }
                df_core::du::DuMessage::Failed { error, .. } => {
                    let message = error.to_string();
                    self.leave_usage(now);
                    self.toasts.error(message, now);
                    return true;
                }
                df_core::du::DuMessage::Started { .. } => {}
            }
        }
        if changed {
            self.apply_usage_sizes();
        }
        changed
    }

    /// Push the walk's numbers into the rows, and re-sort around them.
    ///
    /// The sizes go into [`df_core::fs::Entry::len`] itself rather than being
    /// carried alongside, because that is the field the size *sort* reads — and
    /// the whole point of the mode is that the biggest thing floats to the top
    /// while the walk is still running. Free as a side effect: `Tab`'s size
    /// linemode and the spot panel agree with the bars, because there is one
    /// number.
    fn apply_usage_sizes(&mut self) {
        let Some(usage) = &self.usage else { return };
        // A map, not a list to scan: a walk reports ten times a second and a
        // directory can hold thousands of rows, so a linear search per row
        // would be the one part of this mode that got slower the more there was
        // to measure.
        let weights: HashMap<String, u64> = self
            .tab()
            .cwd
            .dir
            .entries()
            .iter()
            .filter(|entry| entry.is_dir())
            .filter_map(|entry| {
                usage
                    .weight(&entry.name)
                    .map(|weight| (entry.name.clone(), weight.bytes))
            })
            .collect();
        if weights.is_empty() {
            return;
        }
        self.tabs.active_mut().cwd.dir.revise_entries(|entries| {
            let mut changed = false;
            for entry in entries.iter_mut() {
                if let Some(bytes) = weights.get(&entry.name) {
                    if entry.len != *bytes {
                        entry.len = *bytes;
                        changed = true;
                    }
                }
            }
            changed
        });
    }

    /// The `g` chord's bookmarks (PLAN §3's `[goto]` table).
    fn goto(&mut self, slot: u8, now: Instant) {
        let Some(bookmark) = self.config.goto.get(slot as usize) else {
            log::debug!("goto slot {slot} is not in the [goto] table");
            return;
        };
        let path = bookmark.expanded_path();
        // `g 1` / `g 2` — the `sftp://` rows of the shipped table (PLAN §3,
        // §7.6). A bookmark is a string, so a service is reached by writing its
        // URL in `[goto]` and nothing else has to know these two rows are
        // special.
        self.navigate(PathBuf::from(path), now);
    }

    /// Where the open surface's pieces are this frame. Built before the
    /// pointer is looked at, so a click lands on the card rather than on the row
    /// behind it, and reused by the paint so the two cannot disagree.
    fn overlay_geometry(
        &self,
        area: egui::Rect,
        layout: &ui::Layout,
        bar_top: f32,
    ) -> Option<OverlayGeom> {
        match &self.dialog {
            Some(Dialog::Confirm(confirm)) => {
                return Some(OverlayGeom::Confirm(dialog::confirm_geometry(
                    area, confirm,
                )))
            }
            Some(Dialog::Conflict(conflict)) => {
                return Some(OverlayGeom::Conflict(dialog::conflict_geometry(
                    area, conflict,
                )))
            }
            Some(Dialog::Bulk(bulk)) => {
                return Some(OverlayGeom::Bulk(dialog::bulk_geometry(area, bulk)))
            }
            None => {}
        }
        if let Some(picker) = &self.picker {
            let (card, rows) = open::picker_geometry(area, picker.anchor, picker.choices.len());
            return Some(OverlayGeom::Picker(card, rows));
        }
        if self.panel.is_some() {
            let rows = self.task_rows();
            let (card, rects) = panel::geometry(area, bar_top, rows.len());
            return Some(OverlayGeom::Panel(card, rects, rows));
        }
        if let Some(card) = &self.mounts {
            return Some(OverlayGeom::Mounts(crate::mounts::geometry(area, card)));
        }
        if let Some(spot) = &self.spot {
            return Some(OverlayGeom::Spot(spot::geometry(area, bar_top, spot)));
        }
        if let Some(finder) = &self.finder {
            // As many rows as there are, up to the card's cap — an empty
            // result list is a field and a sentence, not a field over eleven
            // rows of nothing.
            let shown = finder.hits.len().min(crate::finder::ROWS);
            return Some(OverlayGeom::Finder(overlay::finder_geometry(area, shown)));
        }
        if let Some(search) = &self.search {
            // The two left columns only: the live preview is half of what this
            // overlay is for, so the pane showing it stays uncovered.
            return Some(OverlayGeom::Search(Box::new(overlay::search_geometry(
                layout.parent.left(),
                layout.list.right(),
                layout.parent.top(),
                bar_top - crate::ui::GAP,
                search.mode,
            ))));
        }
        None
    }

    /// A click on a surface. Pressing a button *is* choosing it — the pointer
    /// does not get a two-step "select, then confirm" the keyboard does not
    /// have.
    fn overlay_click(&mut self, control: Control, page: usize, now: Instant) {
        // The two overlays with a list of results: a click is "this one", the
        // same as arrowing to it and pressing Enter.
        if self.finder.is_some() || self.search.is_some() {
            let Control::PanelRow(offset) = control else {
                return;
            };
            if let Some(finder) = &mut self.finder {
                finder.cursor = (finder.first + offset).min(finder.hits.len().saturating_sub(1));
                self.finder_submit(page, now);
                return;
            }
            if let Some(search) = &mut self.search {
                search.cursor = (search.first + offset).min(search.hits.len().saturating_sub(1));
                self.search_submit(now);
            }
            return;
        }
        // The disks card: a click puts the cursor on the row and acts on it,
        // which is the same one-click-is-Enter rule the opener picker follows.
        if self.mounts.is_some() {
            if let Control::PanelRow(offset) = control {
                if let Some(card) = &mut self.mounts {
                    let index = (card.first + offset).min(card.devices.len().saturating_sub(1));
                    let delta = index as isize - card.cursor as isize;
                    card.move_cursor(delta);
                }
                self.mount_action(now);
            }
            return;
        }
        // The spot's chips are pressed, not selected-then-confirmed: a click on
        // a permission bit flips it, and a click on the checksum starts it.
        if self.spot.is_some() {
            match control {
                Control::Action(index) if index < spot::BITS.len() => {
                    let mode = self
                        .spot
                        .as_ref()
                        .map(|spot| spot::toggle(spot.facts.mode, index));
                    if let (Some(mode), Some(row)) =
                        (mode, self.spot.as_ref().and_then(Spot::perm_row))
                    {
                        // The keyboard follows the pointer: the ring lands on
                        // the chip that was just clicked, so `Space` repeats it.
                        if let Some(spot) = &mut self.spot {
                            spot.select(row);
                            spot.bit = index;
                        }
                        self.set_mode(mode, now);
                    }
                }
                Control::Action(_) => {
                    if let Some(row) = self.spot.as_ref().and_then(Spot::hash_row) {
                        if let Some(spot) = &mut self.spot {
                            spot.select(row);
                        }
                    }
                    self.spot_action(now);
                }
                Control::PanelRow(index) => {
                    if let Some(spot) = &mut self.spot {
                        spot.select(index);
                    }
                }
                // Nothing in the panes and nothing on the menu is reachable
                // while a modal card is up — the hit test above never
                // produces them.
                Control::Row(..)
                | Control::Tab(_)
                | Control::Crumb(_)
                | Control::MenuItem(_)
                | Control::SubmenuItem(_)
                | Control::BasketChip
                | Control::BasketRow(_)
                | Control::BasketRemove(_) => {}
            }
            return;
        }
        match control {
            Control::Action(index) => match &mut self.dialog {
                Some(Dialog::Confirm(_)) => {
                    if index == 0 {
                        self.close_overlay(now);
                    } else {
                        self.submit_overlay(page, now);
                    }
                }
                Some(Dialog::Conflict(conflict)) => {
                    match dialog::ConflictAction::ALL.get(index) {
                        Some(action) => {
                            conflict.set_action(*action);
                            self.submit_overlay(page, now);
                        }
                        // Past the three answers is the apply-to-all toggle.
                        None => conflict.toggle_apply_all(),
                    }
                }
                Some(Dialog::Bulk(_)) => {
                    if index == 0 {
                        self.close_overlay(now);
                    } else {
                        self.submit_overlay(page, now);
                    }
                }
                None => {}
            },
            Control::PanelRow(index) => {
                if let Some(Dialog::Conflict(conflict)) = &mut self.dialog {
                    let delta = index as isize - conflict.cursor as isize;
                    conflict.move_cursor(delta);
                    return;
                }
                if let Some(picker) = &mut self.picker {
                    picker.cursor = index.min(picker.choices.len().saturating_sub(1));
                    self.submit_overlay(page, now);
                    return;
                }
                let rows = self.task_rows();
                if let Some(panel) = &mut self.panel {
                    panel.select(index, rows.len());
                }
            }
            Control::Row(..)
            | Control::Tab(_)
            | Control::Crumb(_)
            | Control::MenuItem(_)
            | Control::SubmenuItem(_)
            | Control::BasketChip
            | Control::BasketRow(_)
            | Control::BasketRemove(_) => {}
        }
    }

    // ── The pointer (PLAN §7.5) ─────────────────────────────────────────────

    /// Rebuild the breadcrumb when the directory has changed under it.
    ///
    /// The crumbs are cached against the path they describe; the **chip is
    /// not**. It is re-derived from the status cache every frame, because it
    /// carries a dirty count that changes while the directory does not — a chip
    /// rebuilt only on navigation would freeze at whatever the repository looked
    /// like when you walked in, which is worse than having no count at all. The
    /// cost is a hash lookup and a short `format!`, and only inside a
    /// repository.
    fn sync_path_bar(&mut self) {
        let cwd = self.cwd();
        // The two virtual locations build their own bars — a service chip and a
        // remote path (PLAN §7.6), or the single word `Trash` (PLAN §7.4).
        // `chrome::crumbs` walks `Path::components`, which would read
        // `sftp://host/srv` as a relative directory called `sftp:` and offer a
        // segment that goes nowhere.
        if let Some(at) = self.remote_at() {
            if self.path_bar.0 != cwd {
                self.repo = None;
                self.path_bar = (cwd, crate::remote::crumbs(&at), None);
            }
            self.path_bar.2 = None;
            return;
        }
        if self.tab().trash.is_some() {
            if self.path_bar.0 != cwd {
                self.repo = None;
                self.path_bar = (cwd, crate::trashview::crumbs(), None);
            }
            self.path_bar.2 = None;
            return;
        }
        if self.path_bar.0 != cwd || self.path_bar.1.is_empty() {
            // The one walk up for `.git` per navigation. See [`App::repo`].
            self.repo = df_core::git::repo_root(&cwd);
            self.path_bar = (cwd, chrome::crumbs(&self.cwd()), None);
            // …and the one *request* per navigation. Deliberately here rather
            // than in `repo_status`: a status that fails for good — a corrupt
            // index, a permission problem — stores nothing, so a `repo_status`
            // that asked whenever it found nothing would spawn a `git status`
            // on every frame, which is once per keystroke and once per mouse
            // move. Asking on arrival and on a watch event is the whole
            // schedule.
            if let Some(root) = self.repo.clone() {
                self.git().refresh(&root);
            }
        }
        self.path_bar.2 = self.branch_chip();
    }

    /// The git front end, started the first time a repository is entered.
    ///
    /// Every caller of this has already established that there is a repository
    /// to ask about — see [`App::repo`] — except the spot panel, which asks
    /// about one path and can afford to.
    fn git(&mut self) -> &df_core::git::Git {
        let waker = self.waker.named("git");
        self.git
            .get_or_insert_with(|| df_core::git::Git::start(Arc::new(move || waker.wake())))
    }

    /// What git currently knows about the directory on screen, queueing a first
    /// scan if there has never been one.
    ///
    /// `None` outside a repository *and* while the first scan is still running:
    /// the rows simply have no dots for that half second, which is the correct
    /// picture of what is known.
    ///
    /// **Reads only.** The scan is asked for in [`App::sync_path_bar`] when the
    /// directory changes and in [`App::git_touched`] when something moves; see
    /// the note there for why this one must not ask.
    fn repo_status(&mut self) -> Option<Arc<df_core::git::RepoStatus>> {
        let root = self.repo.clone()?;
        self.git().status(&root)
    }

    /// The breadcrumb's branch chip, or nothing outside a repository.
    fn branch_chip(&mut self) -> Option<String> {
        let root = self.repo.clone()?;
        let status = self.repo_status();
        let counts = status.as_ref().map(|s| s.counts());
        // The porcelain's spelling when there is one, and `HEAD`'s otherwise —
        // a 41-byte read that answers before the worker does, so the chip is
        // there on the first frame in a directory rather than a second later.
        let branch = match status.as_ref().and_then(|s| s.branch()) {
            Some(name) => name.to_string(),
            None => self.git().branch(&root)?,
        };
        Some(chrome::branch_label(&branch, counts))
    }

    /// Tell git something under `dir` moved.
    ///
    /// Called from the watch-driven rescans, which the watcher has already
    /// coalesced; [`df_core::git::Git::refresh`] dedupes again while a scan is
    /// queued or running, so a `cargo build` under the cursor costs one status,
    /// not one per event.
    fn git_touched(&mut self, dir: &Path) {
        let Some(root) = self.repo.clone() else {
            return;
        };
        if !dir.starts_with(&root) {
            return;
        }
        self.git().refresh(&root);
    }

    /// A wheel roll, routed to the pane it was pointed at.
    fn wheel(
        &mut self,
        points: f32,
        at: egui::Pos2,
        layout: &ui::Layout,
        page: usize,
        parent_page: usize,
        now: Instant,
    ) {
        // One "row" of the list pane, which is a row of *tiles* when the
        // directory is a grid — so a wheel roll travels the same distance down
        // the window in both views.
        let rows = crate::mouse::wheel_rows(points, self.pane_step);
        if rows == 0.0 {
            return;
        }
        let scrolloff = self.mgr.scrolloff;
        if layout.preview.contains(at) {
            // A document scrolls by lines with the same coast; a *rendered*
            // document turns pages instead (see `Pane::wheel`).
            self.preview.wheel(rows, now);
            return;
        }
        if layout.parent.contains(at) {
            if let Some(parent) = &mut self.tabs.active_mut().parent {
                parent.wheel(rows, parent_page, scrolloff, 1, now);
            }
            return;
        }
        // Everywhere else is the list: it is the pane a wheel means when it is
        // not pointed at one of the other two.
        let columns = self.columns;
        if self
            .tabs
            .active_mut()
            .cwd
            .wheel(rows, page, scrolloff, columns, now)
        {
            // Scrolling carries the cursor, and a visual run follows the
            // cursor wherever it goes.
            self.apply_visual();
        }
    }

    /// A primary click on something. Returns where it landed, for the ripple.
    fn click(
        &mut self,
        control: Control,
        double: bool,
        pointer: &Pointer,
        geom: &Geom<'_>,
        now: Instant,
    ) -> egui::Rect {
        match control {
            Control::Row(Column::List, index) => {
                let rect = grid::pane_rect(geom.list, geom.grid.as_ref(), geom.list_scroll, index);
                if pointer.shift {
                    // Shift-click: the run from the cursor to here, the way
                    // every list in every program extends a selection.
                    self.click_range(index);
                } else if pointer.toggle {
                    // Ctrl-click: this row alone, on or off, leaving the rest
                    // of the selection exactly as it is.
                    let dir = self.dir();
                    dir.toggle_selected(index);
                    dir.set_cursor(index);
                } else {
                    self.dir().set_cursor(index);
                    self.apply_visual();
                    if double {
                        // The second click *opens*, and opening is the openers
                        // path — the same one `Enter` takes, so a directory is
                        // entered and a file is launched by its rule.
                        self.open_hovered(now);
                    }
                }
                rect
            }
            Control::Row(Column::Parent, index) => {
                // A click on the parent column is "go there" (PLAN §7.5). One
                // click, not two: the column is a path, and every segment of it
                // is somewhere you have already been.
                let rect = ui::row_rect(geom.parent, geom.parent_scroll, index);
                let target = self
                    .tab()
                    .parent
                    .as_ref()
                    .and_then(|parent| parent.dir.row(index))
                    .filter(|entry| entry.is_dir())
                    .map(|entry| entry.path.clone());
                if let Some(path) = target {
                    self.focus = Focus::List;
                    self.navigate(path, now);
                }
                rect
            }
            Control::Crumb(index) => {
                let rect = geom.crumbs.get(index).copied().unwrap_or(egui::Rect::ZERO);
                if let Some(crumb) = self.path_bar.1.get(index) {
                    let path = crumb.path.clone();
                    if path != self.cwd() {
                        self.focus = Focus::List;
                        self.navigate(path, now);
                    }
                }
                rect
            }
            Control::Tab(index) => {
                if self.tabs.switch_to(index, now) {
                    self.tab_changed(now);
                }
                geom.layout
                    .strip
                    .map(|strip| chrome::tab_rects(strip, geom.tabs))
                    .and_then(|rects| rects.get(index).copied())
                    .unwrap_or(egui::Rect::ZERO)
            }
            Control::MenuItem(_) | Control::SubmenuItem(_) => {
                let rect = geom
                    .menu
                    .as_ref()
                    .and_then(|g| g.rect_of(control))
                    .unwrap_or(egui::Rect::ZERO);
                self.menu_click(control, now);
                rect
            }
            Control::Action(_) | Control::PanelRow(_) => {
                let rect = geom
                    .overlay
                    .as_ref()
                    .and_then(|o| o.rect_of(control))
                    .unwrap_or(egui::Rect::ZERO);
                self.overlay_click(control, geom.page, now);
                rect
            }
            // The basket tray (PLAN §7.1): the chip opens and closes it, a row
            // goes to the file it names, and the `×` takes that file back out.
            Control::BasketChip => {
                self.basket_open = !self.basket_open;
                if self.basket_open {
                    self.prune_basket(now);
                }
                geom.basket.chip
            }
            Control::BasketRemove(index) => {
                let rect = geom
                    .basket
                    .removes
                    .get(index)
                    .copied()
                    .unwrap_or(egui::Rect::ZERO);
                self.basket.remove(self.basket_first + index);
                self.clamp_basket();
                rect
            }
            Control::BasketRow(index) => {
                let rect = geom
                    .basket
                    .rows
                    .get(index)
                    .copied()
                    .unwrap_or(egui::Rect::ZERO);
                if let Some(path) = self.basket.paths().get(self.basket_first + index).cloned() {
                    self.reveal(&path, now);
                }
                rect
            }
        }
    }

    /// Shift-click: select the run between the cursor and `index`.
    ///
    /// The cursor moves to the clicked row afterwards, so a second shift-click
    /// extends from where you just were rather than from where you started —
    /// which is what makes shift-click-shift-click walk a selection down a list.
    fn click_range(&mut self, index: usize) {
        let dir = self.dir();
        let (from, to) = select::range(dir.cursor(), index);
        dir.select_range(from, to, true);
        dir.set_cursor(index);
    }

    /// Middle click: the row's directory in a new tab (PLAN §7.5).
    fn middle_click(&mut self, control: Control, now: Instant) {
        let path = match control {
            Control::Row(Column::List, index) => self
                .tab()
                .cwd
                .dir
                .row(index)
                .map(|entry| (entry.path.clone(), entry.is_dir())),
            Control::Row(Column::Parent, index) => self
                .tab()
                .parent
                .as_ref()
                .and_then(|parent| parent.dir.row(index))
                .map(|entry| (entry.path.clone(), entry.is_dir())),
            _ => None,
        };
        // A *file* opens a tab on the directory it is in, with the cursor on
        // it: "open in a new tab" has to mean something for every row, and the
        // only honest reading for a file is "take me there in a new tab".
        let Some((path, is_dir)) = path else { return };
        let (dir, name) = if is_dir {
            (path, None)
        } else {
            match path.parent() {
                Some(parent) => (
                    parent.to_path_buf(),
                    path.file_name().map(|n| n.to_string_lossy().into_owned()),
                ),
                None => return,
            }
        };
        let (mgr, sort) = (self.mgr.clone(), self.sort());
        if !self.tabs.create(dir, &mgr, sort, &self.scanner, now) {
            self.toasts.notice(
                format!("{} tabs is the maximum", crate::tabs::MAX_TABS),
                now,
            );
            return;
        }
        if let Some(name) = name {
            // The scan is in flight, so the cursor is placed the way `--cwd`
            // places it: remembered and retried as each batch lands.
            self.start_cursor = Some((self.cwd(), name.clone()));
            self.dir().cursor_to_name(&name);
        }
        self.visual = None;
        self.rewatch();
    }

    // ── The context menu (PLAN §7.5) ────────────────────────────────────────

    /// Right click: the menu, about whatever row it landed on.
    fn right_click(&mut self, at: egui::Pos2, over: Option<Control>, layout: &ui::Layout) {
        // Only the list pane has a menu. The parent column's one verb is "go
        // there" and the preview's belong to the file it is showing, and a menu
        // that offered "Move to trash" from either would be a menu about a row
        // the pointer is not on.
        if !layout.list.contains(at) {
            return;
        }
        if let Some(Control::Row(Column::List, index)) = over {
            // The menu is about the row it opened on, so the row becomes the
            // cursor first — otherwise "Rename" would rename something else.
            self.dir().set_cursor(index);
            self.apply_visual();
        }
        self.open_menu(at);
    }

    fn open_menu(&mut self, at: egui::Pos2) {
        let entry = self.tab().cwd.dir.cursor_entry().cloned();
        let openers = entry
            .as_ref()
            .map(|entry| open::choices_for(&self.config, entry))
            .unwrap_or_default();
        let facts = menu::Facts {
            has_row: entry.is_some(),
            is_dir: entry.as_ref().is_some_and(|entry| entry.is_dir()),
            targets: self.targets().len(),
            clipboard: !self.clipboard.is_empty(),
            openers: openers.len(),
            // Only in a real directory: an archive nested inside one has to
            // come out before it can be opened, so offering "Extract here" on
            // it would offer something that cannot be done.
            trash: self.tab().trash.is_some(),
            trashed: self
                .tab()
                .trash
                .as_ref()
                .map(|v| v.items.len())
                .unwrap_or(0),
            archive: self.tab().virtual_kind().is_none()
                && entry
                    .as_ref()
                    .is_some_and(crate::archive::looks_like_archive),
        };
        let names = openers.iter().map(|choice| choice.name.clone()).collect();
        self.menu = Some(Menu::new(at, menu::items(facts), names));
        // The click that opened the menu is not half of a double click on
        // whatever is underneath it.
        self.clicks.reset();
    }

    /// Dismiss it. The menu is *gone* now; only its pixels fade.
    fn close_menu(&mut self, now: Instant) {
        if let Some(menu) = &mut self.menu {
            if menu.closing.is_none() {
                menu.closing = Some(now);
            }
        }
    }

    /// A click on a menu row.
    fn menu_click(&mut self, control: Control, now: Instant) {
        let action = match control {
            Control::MenuItem(index) => match self.menu.as_ref().and_then(|m| m.items.get(index)) {
                Some(item) if !item.enabled => return,
                Some(item) if item.submenu() => {
                    // The chevron row does not *do* anything; it flies the
                    // submenu out, and clicking it again puts it away.
                    if let Some(menu) = &mut self.menu {
                        if menu.submenu {
                            menu.close_submenu();
                        } else {
                            menu.open_submenu();
                        }
                    }
                    return;
                }
                Some(item) => item.action,
                None => return,
            },
            Control::SubmenuItem(index) => menu::Action::OpenWith(index),
            _ => return,
        };
        // Closed *before* the action runs: an action that opens a dialog must
        // not open it behind the menu that asked for it.
        self.close_menu(now);
        self.menu_action(action, now);
    }

    /// The menu's own keys.
    ///
    /// **Matched literally, because df-core's keymap has no `[cmenu]`
    /// context.** The shipped registry has tables for the browser, the input
    /// line, the confirm dialog, the picker, the task panel, the spot card and
    /// the help sheet; a menu that arrived in a UI phase has none, and inventing
    /// one here would put a keymap change in the middle of a paint change. So
    /// these five are handled the way the conflict dialog's answers are (see
    /// [`App::overlay_literal`]) — and they are the five keys every menu
    /// everywhere already has, so there is nothing to configure yet. When
    /// `[cmenu]` lands, this is the one function that changes.
    fn menu_key(&mut self, chord: Chord, now: Instant) {
        let plain = chord.mods.is_none();
        let action = {
            let Some(menu) = &mut self.menu else { return };
            match chord.key {
                Key::ArrowUp if plain => {
                    menu.move_cursor(-1);
                    None
                }
                Key::ArrowDown if plain => {
                    menu.move_cursor(1);
                    None
                }
                Key::ArrowRight if plain => {
                    menu.open_submenu();
                    None
                }
                Key::ArrowLeft if plain => {
                    if !menu.close_submenu() {
                        self.close_menu(now);
                    }
                    None
                }
                Key::Enter if plain => match menu.activate() {
                    // `Enter` on the chevron row opens the submenu rather than
                    // doing nothing, which is what `→` does and what a hand
                    // expects from the row it is sitting on.
                    Some(menu::Action::OpenWithMenu) => {
                        menu.open_submenu();
                        None
                    }
                    other => other,
                },
                Key::Escape => {
                    // One rung of its own: the submenu goes first, then the
                    // menu — the same "one rung at a time" the `Esc` ladder is.
                    if !menu.close_submenu() {
                        self.close_menu(now);
                    }
                    None
                }
                _ => None,
            }
        };
        if let Some(action) = action {
            self.close_menu(now);
            self.menu_action(action, now);
        }
    }

    /// Do what a menu row says. Every arm is a key that already exists.
    fn menu_action(&mut self, action: menu::Action, now: Instant) {
        use menu::Action as A;
        // The menu is a **second dispatch** over the same verbs, and it used to
        // walk straight past the gates the keyboard goes through — so "Open
        // with…" on a remote row launched a viewer on the string
        // `sftp://host/photo.png`, and only `d` had a hand-written branch
        // keeping it honest. A row that is a command is now put through the
        // same one gate, so the next row added to the menu cannot forget.
        if let Some(command) = menu_command(action) {
            if self.refuse_where_we_are(command, now) {
                return;
            }
        }
        match action {
            A::Open => self.open_hovered(now),
            A::OpenWithMenu => {}
            A::OpenWith(index) => {
                let Some(entry) = self.tab().cwd.dir.cursor_entry().cloned() else {
                    return;
                };
                // Re-derived rather than carried: `choices_for` is a pure
                // function of the config and the entry, and holding a copy in
                // the menu would be a second place for it to be wrong.
                let choices = open::choices_for(&self.config, &entry);
                if let Some(choice) = choices.get(index).cloned() {
                    self.launch(&choice, self.targets(), now);
                }
            }
            A::ExtractHere => self.extract(false, now),
            A::ExtractSubfolder => self.extract(true, now),
            A::Yank => self.set_clipboard(false, now),
            A::Cut => self.set_clipboard(true, now),
            A::Paste => self.paste(false, now),
            A::Rename => self.open_rename(false, now),
            // The same choice `d` makes, and it still has to be made here:
            // `Trash` is deliberately *not* inert remotely — it becomes a
            // `RemoteDelete` — so the gate above lets it through, and a plain
            // `ConfirmKind::Trash` would hand a `TrashJob` a list of `sftp://…`
            // strings, which are *relative* `PathBuf`s resolved against the
            // process's own directory.
            A::Trash => {
                let kind = if self.tab().remote.is_some() {
                    ConfirmKind::RemoteDelete
                } else {
                    ConfirmKind::Trash
                };
                self.open_confirm(kind, now);
            }
            A::CopyPath => self.copy_piece(Piece::Path, now),
            A::CopyName => self.copy_piece(Piece::Filename, now),
            A::Properties => self.toggle_spot(),
            A::Restore => self.trash_restore(now),
            A::Purge => self.open_confirm(ConfirmKind::Purge, now),
            A::EmptyTrash => self.open_confirm(ConfirmKind::EmptyTrash, now),
        }
    }

    // ── Band select (PLAN §7.5) ─────────────────────────────────────────────

    /// The pointer has moved with the button down.
    fn drag(
        &mut self,
        at: Option<egui::Pos2>,
        list: egui::Rect,
        strip: Option<egui::Rect>,
        scroll_rows: f32,
        metrics: Option<grid::Metrics>,
        now: Instant,
    ) {
        let (Some(at), Some(press)) = (at, self.press) else {
            return;
        };
        // A tab in the hand owns the gesture: the pointer is carrying a chip,
        // not drawing a band and not holding files.
        if let Some(tab_drag) = &mut self.tab_drag {
            tab_drag.at = at;
            return;
        }
        if !press.dragging {
            if (at - press.at).length() < crate::mouse::DRAG_THRESHOLD {
                return;
            }
            if let Some(press) = &mut self.press {
                press.dragging = true;
            }
            if let (Some(index), Some(strip)) = (press.on_tab, strip) {
                // PLAN §2: "drag a tab out to spawn a window". The chip is
                // picked up here and the decision is made on release, by
                // [`crate::window::release`].
                self.begin_tab_drag(index, press.at, at, strip);
                return;
            }
            if press.on_basket {
                // PLAN §7.1: "drag the whole basket as one payload". The same
                // drag machinery, given a different set of paths — so the
                // ghost, the target highlighting, the modifier badges, the
                // spring-back and the Wayland hand-off are all the ones that
                // already work.
                self.drag_basket(press.at, at, now);
                return;
            }
            if press.on_row {
                // **The seam.** A drag that began on a row is a *file* drag,
                // never a band select: a drag from a row is how every file
                // manager moves files.
                self.begin_drag(press.at, at, list, scroll_rows, metrics.as_ref(), now);
                return;
            }
            if !press.in_list {
                return;
            }
            self.band = Some(select::Band::new(press.at));
        }
        let Some(origin) = self.band.as_ref().map(|band| band.origin) else {
            return;
        };
        let rows = self.tab().cwd.dir.len();
        let band = crate::mouse::band(origin, at);
        let run = match &metrics {
            Some(metrics) => grid::band_items(list, metrics, scroll_rows, rows, band),
            None => crate::mouse::band_rows(list, scroll_rows, rows, ui::ROW_HEIGHT, band),
        };
        self.apply_band(run);
    }

    /// Bring the selection in line with the band, live.
    ///
    /// The same arithmetic visual mode uses ([`select::range_delta`]), and for
    /// the same reason: a band that shrinks has to hand back a selection that
    /// was there before the drag started, not clear it.
    fn apply_band(&mut self, run: Option<(usize, usize)>) {
        let Some(band) = self.band.as_mut() else {
            return;
        };
        let dir = &mut self.tabs.active_mut().cwd.dir;
        match run {
            Some(wanted) => {
                let (leaving, entering) = select::range_delta(band.applied, wanted);
                for position in leaving {
                    let Some(name) = dir.row(position).map(|entry| entry.name.clone()) else {
                        continue;
                    };
                    let was = band.was_selected(&name).unwrap_or(false);
                    dir.select_range(position, position, was);
                }
                for position in entering {
                    let Some(name) = dir.row(position).map(|entry| entry.name.clone()) else {
                        continue;
                    };
                    band.remember(&name, dir.is_selected(&name));
                    dir.select_range(position, position, true);
                }
                band.applied = Some(wanted);
            }
            // The band has left the listing entirely — dragged above the first
            // row, or into a directory that has none.
            None => {
                if let Some((from, to)) = band.applied.take() {
                    for position in from..=to {
                        let Some(name) = dir.row(position).map(|entry| entry.name.clone()) else {
                            continue;
                        };
                        let was = band.was_selected(&name).unwrap_or(false);
                        dir.select_range(position, position, was);
                    }
                }
            }
        }
    }

    /// `Esc` mid-drag: every row the band touched goes back to what it was.
    fn cancel_band(&mut self) {
        let Some(band) = self.band.take() else { return };
        let dir = &mut self.tabs.active_mut().cwd.dir;
        for (name, was) in &band.prior {
            if let Some(position) = dir.position_of(name) {
                dir.select_range(position, position, *was);
            }
        }
        // The press is spent as well, or letting go would start it again.
        self.press = None;
    }

    // ── Drag and drop (PLAN §7.1) ───────────────────────────────────────────

    /// The basket's chip has been dragged: pick up everything in it.
    ///
    /// Deliberately a separate entry point from [`App::begin_drag`] and not a
    /// parameter on it: that one is about a *row*, and half of it (the pane
    /// geometry, the index, the "is the grabbed row in the selection" rule) has
    /// no meaning here. What they share is the `Drag` they build, which is the
    /// part that matters.
    fn drag_basket(&mut self, from: egui::Pos2, at: egui::Pos2, now: Instant) {
        let paths = self.basket.paths().to_vec();
        if paths.is_empty() {
            return;
        }
        let label = paths
            .first()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "basket".to_string());
        self.drag = Some(Drag {
            paths,
            label,
            // The generic file glyph: a basket holds whatever it holds, and a
            // card wearing the first file's icon would claim they are all that
            // kind.
            icon: crate::icons::generic(&self.palette, self.nerd),
            home: from,
            at,
            spring: dnd::SpringOpen::default(),
            last: now,
            handed_off: false,
        });
    }

    /// A press on a row has travelled far enough to be a drag. Pick the files
    /// up.
    ///
    /// **The selection is not changed.** Dragging a row that is not in the
    /// selection drags that row alone and leaves the selection where it was —
    /// which is what every file manager does, and the alternative (a drag that
    /// silently re-selects) would make "drag this one file out of a marked set"
    /// impossible to express.
    fn begin_drag(
        &mut self,
        from: egui::Pos2,
        at: egui::Pos2,
        content: egui::Rect,
        scroll_rows: f32,
        metrics: Option<&grid::Metrics>,
        now: Instant,
    ) {
        if self.tab().archive.is_some() || self.tab().remote.is_some() {
            // The rows inside an archive name paths that do not exist, and a
            // remote row names a URL no other application can open; a drag out
            // of either would offer a `text/uri-list` of fictions (PLAN §7.1,
            // §7.3, §7.6). Extract, or `y` then `p` in a local folder, first.
            // The trash is exempt: its rows are real files.
            return;
        }
        let dir = &self.tab().cwd.dir;
        let Some(index) = grid::pane_at(content, metrics, scroll_rows, dir.len(), from) else {
            return;
        };
        let Some(entry) = dir.row(index) else { return };
        let grabbed = entry.path.clone();
        let label = entry.name.clone();
        let icon = crate::icons::icon_for(entry, &self.theme, &self.palette, self.nerd);
        let selected = dir.selected_paths();
        let paths = if selected.contains(&grabbed) {
            selected
        } else {
            vec![grabbed]
        };
        self.drag = Some(Drag {
            paths,
            label,
            icon,
            home: grid::pane_rect(content, metrics, scroll_rows, index).center(),
            at,
            spring: dnd::SpringOpen::default(),
            last: now,
            handed_off: false,
        });
        // A drag that starts is a drag the cancel of an older one has nothing
        // to say about.
        self.spring_back = None;
    }

    // ── Windows (PLAN §2) ───────────────────────────────────────────────────

    /// Open a window on `dir` — `Ctrl+N`, and the far end of a tab dragged out
    /// of the strip.
    ///
    // VERIFY-LIVE: `Ctrl+N` in a directory, then a file dragged from the new
    // window onto a folder row in the old one. The second window should open on
    // the same directory with its own tab strip; the drag between them should
    // ring the *receiving* window's edge (the drop-in path, not the local-drag
    // path — that is what [`crate::dnd::is_ours`]'s pid is for) and paste with
    // the usual conflict dialog and undo toast.
    ///
    /// A whole new process, for the reasons set out in [`crate::window`]. The
    /// failure is a toast rather than a log line because there is nothing else
    /// on screen to notice: a window that did not appear looks exactly like a
    /// key that was not pressed.
    fn open_window(&mut self, dir: &Path, now: Instant) -> bool {
        match self.windows.open(dir) {
            Ok(()) => {
                self.toasts
                    .notice(format!("New window: {}", file_name(dir)), now);
                true
            }
            Err(e) => {
                log::warn!("could not open a window on {}: {e}", dir.display());
                self.toasts
                    .error(format!("Could not open a new window: {e}"), now);
                false
            }
        }
    }

    /// The local directory a given tab would open a window on.
    ///
    /// [`App::local_origin`]'s rule, applied to a tab that is not necessarily
    /// the active one: a window is a second process, and an archive's inner
    /// path, a trash entry or a remote URL are not things a second process can
    /// be started in.
    fn tab_origin(&self, index: usize) -> Option<PathBuf> {
        let tab = self.tabs.iter().nth(index)?;
        if let Some(session) = &tab.remote {
            return Some(session.origin.clone());
        }
        if let Some(view) = &tab.trash {
            return Some(view.origin.clone());
        }
        if let Some(browse) = &tab.archive {
            return Some(browse.real());
        }
        Some(tab.cwd.path().to_path_buf())
    }

    /// A press on a tab chip has travelled far enough to be a gesture: pick the
    /// chip up.
    ///
    /// The ghost appears at [`crate::mouse::DRAG_THRESHOLD`] like every other
    /// drag in the program, and only *arms* at
    /// [`crate::window::DETACH_THRESHOLD`] — so the hand sees it has hold of
    /// something long before the gesture can do anything, which is what makes
    /// the big threshold feel like a decision rather than a dead zone.
    fn begin_tab_drag(
        &mut self,
        index: usize,
        from: egui::Pos2,
        at: egui::Pos2,
        strip: egui::Rect,
    ) {
        let Some(label) = self.tabs.iter().nth(index).map(Tab::title) else {
            return;
        };
        let home = chrome::tab_rects(strip, self.tabs.len())
            .get(index)
            .map(egui::Rect::center)
            .unwrap_or(from);
        self.tab_drag = Some(TabDrag {
            tab: index,
            from,
            at,
            home,
            label,
            icon: crate::icons::folder(&self.palette, self.nerd),
        });
        // A gesture that starts is one the cancel of an older one has nothing
        // to say about.
        self.spring_back = None;
    }

    // VERIFY-LIVE: with two tabs open, press a chip and pull it downwards. The
    // ghost should appear within a few pixels of travel, pick up the "New
    // window" chip as it clears the strip, and on release open a window on that
    // tab's directory and leave one fewer chip behind. Let go inside the strip,
    // or press `Esc` mid-gesture, and the ghost should fly back to its chip
    // with nothing opened and nothing closed.
    /// Letting go of a tab chip (PLAN §2).
    ///
    /// Detaching the **last** tab is a notice rather than an action: with one
    /// process per window there is no window identity to move — closing the tab
    /// would quit this window and opening the new one would put the same
    /// directory back on screen, which is a lot of flicker to achieve nothing.
    /// `Ctrl+N` is what that gesture meant, and the notice says so.
    fn release_tab_drag(&mut self, at: egui::Pos2, strip: egui::Rect, now: Instant) {
        let Some(drag) = self.tab_drag.take() else {
            return;
        };
        if crate::window::release(drag.from, at, strip) == crate::window::Release::SpringBack {
            self.spring_tab_home(drag, at, now);
            return;
        }
        if self.tabs.len() <= 1 {
            self.toasts
                .notice("This is the only tab — Ctrl+N opens another window", now);
            self.spring_tab_home(drag, at, now);
            return;
        }
        let Some(dir) = self.tab_origin(drag.tab) else {
            self.spring_tab_home(drag, at, now);
            return;
        };
        // The tab only closes if the window really opened: a detach that lost
        // the tab *and* failed to show it anywhere would be the one outcome
        // this gesture may never have.
        if self.open_window(&dir, now) && self.tabs.close(drag.tab) {
            self.tab_changed(now);
        }
    }

    /// The tab ghost flies back to its chip: `Esc`, or a release that did not
    /// arm. The same spring the file ghost uses (`delightful-ui` §6).
    fn spring_tab_home(&mut self, drag: TabDrag, at: egui::Pos2, now: Instant) {
        self.spring_back = Some(SpringHome {
            spring: dnd::SpringBack::new(at, drag.home, 1, now),
            label: drag.label,
            icon: drag.icon,
        });
    }

    /// One frame of a live drag: where it is, what it is over, and whether it
    /// has ended. Returns what the painter needs.
    fn tick_drag(
        &mut self,
        zones: &dnd::Zones,
        pointer: &Pointer,
        window: egui::Rect,
        pages: (usize, usize),
        now: Instant,
    ) -> Option<DragFrame> {
        let drag = self.drag.as_ref()?;
        if drag.handed_off {
            // The compositor has the pointer. Nothing local moves the drag now;
            // it ends when [`crate::wayland`] says it did.
            return None;
        }
        let verb = dnd::verb_for(pointer.toggle, pointer.alt);
        let at = pointer.at.unwrap_or(drag.at);
        let dt = now
            .saturating_duration_since(drag.last)
            .as_secs_f32()
            .min(0.25);
        if let Some(drag) = self.drag.as_mut() {
            drag.at = at;
            drag.last = now;
        }

        // Out of the window: hand it to the compositor and stop drawing it.
        // The implicit pointer grab means motion keeps arriving even outside
        // our own surface, so "left the window" is a question this side can
        // still answer — and the moment it becomes true is the moment the
        // pointer stops being ours (see [`crate::wayland`]).
        if !window.contains(at) && pointer.down {
            self.hand_off_drag(now);
            return None;
        }

        // Where a drop would land. Resolved from this frame's geometry, never
        // from the highlight, because a highlight is an animation and a drop is
        // a commitment.
        let target = {
            let tab = self.tab();
            dnd::target_at(zones, at, |column, index| match column {
                Column::List => tab.cwd.dir.row(index).is_some_and(|e| e.is_dir()),
                Column::Parent => tab
                    .parent
                    .as_ref()
                    .and_then(|p| p.dir.row(index))
                    .is_some_and(|e| e.is_dir()),
            })
        };
        let dest = target.and_then(|target| self.dest_of(target));
        let paths = self
            .drag
            .as_ref()
            .map(|d| d.paths.clone())
            .unwrap_or_default();
        let valid = dest
            .as_deref()
            .is_some_and(|dest| dnd::valid_dest(dest, &paths, verb));
        let lit = target.filter(|_| valid);

        // The hold-to-open timer, and the two panes' edge bands.
        if let Some(drag) = self.drag.as_mut() {
            drag.spring.aim(lit.filter(|t| t.is_row()), now);
        }
        let sprung = self
            .drag
            .as_mut()
            .and_then(|drag| drag.spring.fired(now))
            .and_then(|target| self.dest_of(target));
        if let Some(dest) = sprung {
            // macOS's spring-loaded folder: the pane you are dragging over
            // *goes there*, so a drag can reach anywhere without being let go.
            self.navigate(dest, now);
        }
        self.autoscroll(zones, at, dt, pages, now);

        // The release is the drop.
        if pointer.released {
            let drag = self.drag.take();
            self.targets.tick(None, None, now);
            match (drag, dest.filter(|_| valid)) {
                (Some(_), Some(dest)) => self.drop_here(&paths, &dest, verb, now),
                (Some(drag), None) => self.spring_home(drag, now),
                (None, _) => {}
            }
            return None;
        }
        Some(DragFrame {
            at,
            verb,
            target: lit,
        })
    }

    /// Where a target's files would go.
    fn dest_of(&self, target: dnd::Target) -> Option<PathBuf> {
        match target {
            dnd::Target::Row(Column::List, index) => self
                .tab()
                .cwd
                .dir
                .row(index)
                .map(|entry| entry.path.clone()),
            dnd::Target::Row(Column::Parent, index) => self
                .tab()
                .parent
                .as_ref()
                .and_then(|parent| parent.dir.row(index))
                .map(|entry| entry.path.clone()),
            dnd::Target::Pane(Column::List) => Some(self.cwd()),
            dnd::Target::Pane(Column::Parent) => self
                .tab()
                .parent
                .as_ref()
                .map(|parent| parent.path().to_path_buf()),
            dnd::Target::Crumb(index) => self.path_bar.1.get(index).map(|crumb| crumb.path.clone()),
            dnd::Target::Tab(index) => self
                .tabs
                .iter()
                .nth(index)
                .map(|tab| tab.cwd.path().to_path_buf()),
        }
    }

    /// Scroll a listing the drag is hanging over the edge of.
    ///
    /// Through [`crate::tab::Listing::wheel`], which is the one place in the
    /// program that moves a view: it carries the sub-row remainder and drags
    /// the cursor along, so the scrolloff rule does not undo the travel on the
    /// next frame (the same subtlety the wheel documents).
    fn autoscroll(
        &mut self,
        zones: &dnd::Zones,
        at: egui::Pos2,
        dt: f32,
        pages: (usize, usize),
        now: Instant,
    ) {
        let scrolloff = self.mgr.scrolloff;
        let list = dnd::autoscroll(zones.list_content, at) * dt;
        if list != 0.0 {
            let columns = self.columns;
            self.tabs
                .active_mut()
                .cwd
                .wheel(list, pages.0, scrolloff, columns, now);
        }
        let parent = dnd::autoscroll(zones.parent_content, at) * dt;
        if parent != 0.0 {
            if let Some(pane) = &mut self.tabs.active_mut().parent {
                pane.wheel(parent, pages.1, scrolloff, 1, now);
            }
        }
    }

    /// Carry out a drop, through the pipeline `y`/`x`/`p` and `-` already use.
    ///
    /// Nothing here is new machinery: a move is a cut pasted somewhere else, a
    /// copy is a yank pasted somewhere else, and a link is `-` aimed at another
    /// directory. Conflicts open the same dialog, the journal gets the same
    /// records, and `u` takes any of it back.
    fn drop_here(&mut self, paths: &[PathBuf], dest: &Path, verb: dnd::Verb, now: Instant) {
        // PLAN §7.1's target seam already hands this function a `Vec<PathBuf>`
        // and a destination, and PLAN §7.6's `p` already reads its meaning off
        // the two ends — so **dropping local files onto a remote pane uploads
        // them** with no drop-specific code at all (`paste_into` routes it).
        //
        // The one thing that does need saying: a *move* onto a remote service
        // would have to delete the local originals after an upload, and an
        // upload that half-succeeded would then delete files that never
        // arrived. So a drop onto a remote destination is always a copy, and
        // the notice says so rather than leaving the user to notice their
        // originals are still here.
        let verb = match (verb, crate::remote::at_of(dest)) {
            (dnd::Verb::Move, Some(_)) => {
                self.toasts.notice(
                    "Uploaded as a copy — a move to a server is not supported",
                    now,
                );
                dnd::Verb::Copy
            }
            (dnd::Verb::Link, Some(_)) => {
                self.toasts.notice("Cannot link onto a server", now);
                return;
            }
            (verb, _) => verb,
        };
        match verb {
            dnd::Verb::Move => {
                let clip = Clipboard::cut(paths.to_vec());
                self.paste_into(&clip, dest.to_path_buf(), false, now);
            }
            dnd::Verb::Copy => {
                let clip = Clipboard::yank(paths.to_vec());
                self.paste_into(&clip, dest.to_path_buf(), false, now);
            }
            // The same default `-` has: a symlink, because a hard link across
            // filesystems is not a thing and a drag crosses them freely.
            dnd::Verb::Link => self.link_into(
                paths.to_vec(),
                dest.to_path_buf(),
                Some(LinkKind::Absolute),
                now,
            ),
        }
    }

    /// `Esc`, or a release over nothing: the ghost flies home and nothing
    /// happens.
    fn spring_home(&mut self, drag: Drag, now: Instant) {
        self.spring_back = Some(SpringHome {
            spring: dnd::SpringBack::new(drag.at, drag.home, drag.paths.len(), now),
            label: drag.label,
            icon: drag.icon,
        });
    }

    /// The pointer has left the window: give the drag to the compositor.
    ///
    // VERIFY-LIVE: the handoff itself. The ghost should vanish at the window's
    // edge and the compositor's drag icon should appear in its place, in one
    // motion with no gap — and dragging back *in* should light the targets
    // again through the data device rather than through egui.
    fn hand_off_drag(&mut self, now: Instant) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        drag.handed_off = true;
        let paths = drag.paths.clone();
        let count = paths.len();
        let Some(device) = &self.data_device else {
            // No protocol, so there is nowhere for the drag to go. It is not
            // an error and it is not silence either: the ghost springs home,
            // which is what "that did not happen" looks like everywhere else
            // in this gesture.
            if let Some(drag) = self.drag.take() {
                self.spring_home(drag, now);
            }
            return;
        };
        let scale = self
            .gfx
            .as_ref()
            .map(|gfx| gfx.egui_ctx.pixels_per_point().round() as i32)
            .unwrap_or(1);
        let rgba =
            |color: egui::Color32| crate::wayland::Rgba(color.r(), color.g(), color.b(), color.a());
        device.drag(
            dnd::offer(&paths),
            count,
            rgba(self.palette.surface1),
            rgba(self.palette.text),
            scale,
        );
    }

    /// What [`crate::wayland`] has to say, once a frame.
    fn poll_data_device(&mut self, now: Instant) {
        let Some(device) = &self.data_device else {
            return;
        };
        for event in device.poll() {
            match event {
                crate::wayland::Event::Enter { at, ours } => {
                    self.incoming = Some(Incoming {
                        at: egui::pos2(at.0, at.1),
                        ours,
                    });
                }
                crate::wayland::Event::Motion { at } => {
                    if let Some(incoming) = &mut self.incoming {
                        incoming.at = egui::pos2(at.0, at.1);
                    }
                }
                crate::wayland::Event::Leave => self.incoming = None,
                crate::wayland::Event::Drop { paths, ours } => {
                    let at = self.incoming.take().map(|incoming| incoming.at);
                    self.take_external_drop(paths, ours, at, now);
                }
                crate::wayland::Event::DragEnded => {
                    // Our own drag, back from the compositor. Whatever it did
                    // out there, this side is done holding files.
                    self.drag = None;
                    self.press = None;
                    self.targets.tick(None, None, now);
                }
            }
        }
    }

    /// A drop from another application (PLAN §7.1's "drop in").
    ///
    /// Always a **copy**, whatever the source thought it was offering: the file
    /// is somebody else's until they say otherwise, and a drop that moved files
    /// out of another program's directory on its own initiative is not a
    /// behaviour a file manager gets to have.
    fn take_external_drop(
        &mut self,
        paths: Vec<PathBuf>,
        ours: bool,
        at: Option<egui::Pos2>,
        now: Instant,
    ) {
        if ours {
            // Our own drag, dropped back on our own window after a trip
            // through the compositor. The local gesture is over — and the verb
            // went with it: once the compositor owns the pointer nothing tells
            // this side which modifiers are held, so the drop takes the same
            // copy every other external drop does rather than guessing at a
            // move it cannot undo the guess for.
            self.drag = None;
        }
        if paths.is_empty() {
            self.toasts
                .notice("Nothing in that drop this can open", now);
            return;
        }
        let dest = at
            .and_then(|at| self.dropped_target(at))
            .unwrap_or_else(|| self.cwd());
        let clip = Clipboard::yank(paths);
        self.paste_into(&clip, dest, false, now);
    }

    /// Where an external drop at `at` lands.
    ///
    /// Recomputed from the *last frame's* geometry, which is the only geometry
    /// there is at the moment a `wl_data_device.drop` arrives — the frame that
    /// drew the highlight the user was aiming at.
    fn dropped_target(&self, at: egui::Pos2) -> Option<PathBuf> {
        let target = dnd::target_at(self.zones.as_ref()?, at, |column, index| match column {
            Column::List => self.tab().cwd.dir.row(index).is_some_and(|e| e.is_dir()),
            Column::Parent => self
                .tab()
                .parent
                .as_ref()
                .and_then(|p| p.dir.row(index))
                .is_some_and(|e| e.is_dir()),
        })?;
        self.dest_of(target)
    }

    // ── The system clipboard (PLAN §7.4) ────────────────────────────────────

    /// `Y`: the native port of `clipboard.sh`.
    fn yank_to_system(&mut self, now: Instant) {
        let paths = self.targets();
        if paths.is_empty() {
            self.toasts.notice("Nothing to copy", now);
            return;
        }
        let single = paths.first().filter(|_| paths.len() == 1).cloned();
        let (mime, size) = match &single {
            Some(path) => self.type_of(path),
            None => (df_core::fs::mime::UNKNOWN_MIME, 0),
        };
        match crate::clipboard::branch_for(paths.len(), mime, size) {
            crate::clipboard::Branch::Image(mime) => {
                let Some(path) = single else { return };
                let Some(bytes) = self.read_for_clipboard(&path, now) else {
                    return;
                };
                let label = crate::clipboard::image_label(mime);
                self.offer(Some(mime), &bytes, format!("Copied image ({label})"), now);
            }
            crate::clipboard::Branch::Text => {
                let Some(path) = single else { return };
                let Some(bytes) = self.read_for_clipboard(&path, now) else {
                    return;
                };
                let name = file_name(&path);
                self.offer(None, &bytes, format!("Copied text: {name}"), now);
            }
            crate::clipboard::Branch::Uris => {
                let list = crate::clipboard::uri_list(&paths);
                let message = match &single {
                    Some(path) => format!("Copied file reference: {}", file_name(path)),
                    None => format!("Copied {} paths", paths.len()),
                };
                self.offer(Some("text/uri-list"), list.as_bytes(), message, now);
            }
        }
    }

    /// `c t`: the native port of `copy-text.sh`.
    fn copy_file_text(&mut self, now: Instant) {
        let paths = self.targets();
        let single = paths.first().filter(|_| paths.len() == 1).cloned();
        let Some(path) = single else {
            // The script's own fallback: more than one file (or none) is a
            // plain yank, because there is no such thing as the text of two
            // files.
            self.set_clipboard(false, now);
            if !paths.is_empty() {
                self.toasts
                    .notice("Yanked — c t copies one file's text", now);
            }
            return;
        };
        let (mime, _) = self.type_of(&path);
        if !crate::clipboard::is_text_like(mime) {
            self.set_clipboard(false, now);
            self.toasts
                .notice(format!("Yanked (binary): {}", file_name(&path)), now);
            return;
        }
        let Some(bytes) = self.read_for_clipboard(&path, now) else {
            return;
        };
        let name = file_name(&path);
        self.offer(None, &bytes, format!("Copied text contents: {name}"), now);
    }

    /// `c c` / `c d` / `c f` / `c n`.
    fn copy_piece(&mut self, piece: Piece, now: Instant) {
        let Some(path) = self
            .tab()
            .cwd
            .dir
            .cursor_entry()
            .map(|entry| entry.path.clone())
        else {
            self.toasts.notice("Nothing under the cursor", now);
            return;
        };
        let text = match piece {
            Piece::Path => path.to_string_lossy().into_owned(),
            Piece::Dirname => path
                .parent()
                .unwrap_or(Path::new("/"))
                .to_string_lossy()
                .into_owned(),
            Piece::Filename => file_name(&path),
            Piece::Stem => path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let what = match piece {
            Piece::Path => "path",
            Piece::Dirname => "directory",
            Piece::Filename => "name",
            Piece::Stem => "stem",
        };
        self.offer(None, text.as_bytes(), format!("Copied {what}"), now);
    }

    /// The spot panel's `c c`: the focused row's value.
    fn copy_spot_cell(&mut self, now: Instant) {
        let Some((label, text)) = self.spot.as_ref().and_then(Spot::cell_text) else {
            self.toasts.notice("Nothing to copy on this row", now);
            return;
        };
        self.offer(
            None,
            text.as_bytes(),
            format!("Copied {}", label.to_lowercase()),
            now,
        );
    }

    /// Hand bytes to the clipboard and say what happened, either way.
    fn offer(&mut self, mime: Option<&str>, bytes: &[u8], message: String, now: Instant) {
        match crate::clipboard::copy(mime, bytes) {
            Ok(()) => self.toasts.notice(message, now),
            Err(error) => self.clip_failed(error, now),
        }
    }

    fn clip_failed(&mut self, error: crate::clipboard::ClipError, now: Instant) {
        match error {
            // `wl-clipboard` not being installed is not something the user did,
            // so it is a plain notice rather than a red bar.
            crate::clipboard::ClipError::Missing(_) => self.toasts.notice(error.to_string(), now),
            crate::clipboard::ClipError::Failed(_) => self.toasts.error(error.to_string(), now),
        }
    }

    /// What a path is, and how big — the two inputs to
    /// [`crate::clipboard::branch_for`].
    ///
    /// The mime is sniffed from the file's own bytes rather than from its name,
    /// which is what `file -b --mime-type` does in the script; the *hint* is the
    /// listing's extension guess, which is what keeps a `.rs` reading as source
    /// rather than as plain text.
    fn type_of(&self, path: &Path) -> (&'static str, u64) {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if path.is_dir() {
            // A directory has no contents to put on a clipboard, so it is
            // always a reference.
            return (df_core::fs::mime::UNKNOWN_MIME, size);
        }
        let hint = self
            .tab()
            .cwd
            .dir
            .entries()
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.mime)
            .unwrap_or(df_core::fs::mime::UNKNOWN_MIME);
        let mime = df_core::preview::sniff_file(path, hint).unwrap_or(hint);
        (mime, size)
    }

    /// Read a file whose contents are going on the clipboard.
    ///
    /// Blocking, on the event loop, and bounded by
    /// [`crate::clipboard::SIZE_CAP`] — the branch that got here has already
    /// refused anything larger. Reading it on a worker would mean a clipboard
    /// that is set some time after the key was pressed, which is a race with
    /// whatever the user pastes into next.
    fn read_for_clipboard(&mut self, path: &Path, now: Instant) -> Option<Vec<u8>> {
        match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                self.toasts
                    .error(format!("{}: {error}", path.display()), now);
                None
            }
        }
    }

    /// `p` with nothing yanked: whatever the *system* clipboard is holding.
    ///
    /// **Precedence, stated once.** The internal clipboard always wins: `y`
    /// then `p` must paste what `y` yanked, whatever some other application has
    /// put on the system clipboard since. This is the fallback for the empty
    /// case, which is also the case where the old behaviour was a notice saying
    /// there was nothing to paste.
    fn paste_system(&mut self, force: bool, now: Instant) {
        let types = match crate::clipboard::offered_types() {
            Ok(types) => types,
            Err(error) => {
                self.clip_failed(error, now);
                return;
            }
        };
        match crate::clipboard::choose_offer(&types) {
            None => self.toasts.notice("Nothing yanked — y copies, x cuts", now),
            Some(crate::clipboard::Offer::Files) => self.paste_clipboard_files(force, now),
            Some(crate::clipboard::Offer::Image(mime)) => {
                let extension = crate::clipboard::image_extension(&mime);
                self.save_clipboard(&mime, extension, now);
            }
            Some(crate::clipboard::Offer::Text(mime)) => self.save_clipboard(&mime, "txt", now),
        }
    }

    fn paste_clipboard_files(&mut self, force: bool, now: Instant) {
        let bytes = match crate::clipboard::paste("text/uri-list") {
            Ok(bytes) => bytes,
            Err(error) => {
                self.clip_failed(error, now);
                return;
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        let offered = crate::clipboard::parse_uri_list(&text);
        let count = offered.len();
        // Cross-checked against the filesystem: a clipboard outlives the files
        // in it, and a paste plan built over a path that is gone fails halfway
        // through instead of before it starts.
        let paths: Vec<PathBuf> = offered.into_iter().filter(|path| path.exists()).collect();
        if paths.is_empty() {
            self.toasts.notice(
                if count == 0 {
                    "The clipboard has no files on it".to_string()
                } else {
                    format!("{} clipboard {} no longer there", count, plural_verb(count))
                },
                now,
            );
            return;
        }
        if paths.len() < count {
            self.toasts.notice(
                format!(
                    "{} of {count} clipboard files are gone",
                    count - paths.len()
                ),
                now,
            );
        }
        let clipboard = Clipboard::yank(paths);
        self.paste_from(&clipboard, force, now);
    }

    /// Save what the clipboard is holding as a file in this directory.
    ///
    /// The one paste that is a *local write* rather than a plan: `p` with an
    /// image on the system clipboard lands a file with `std::fs::write`, so
    /// unlike the file paste (which reads its meaning off both ends and can
    /// become an upload) it needs the pane to be a directory on this machine.
    fn save_clipboard(&mut self, mime: &str, extension: &str, now: Instant) {
        let cwd = self.cwd();
        if !scannable(&cwd) {
            self.toasts
                .notice("The clipboard can only be saved into a local folder", now);
            return;
        }
        let bytes = match crate::clipboard::paste(mime) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.clip_failed(error, now);
                return;
            }
        };
        if bytes.is_empty() {
            self.toasts.notice("The clipboard is empty", now);
            return;
        }
        let name = format!(
            "clipboard_{}.{extension}",
            crate::format::file_stamp(std::time::SystemTime::now())
        );
        let path = cwd.join(&name);
        if let Err(error) = std::fs::write(&path, &bytes) {
            self.toasts
                .error(format!("{}: {error}", path.display()), now);
            return;
        }
        // Journalled as a create, so `u` takes it back — a paste that made a
        // file and could not be undone would be the one mutation in the program
        // without an inverse.
        if let Ok(fingerprint) = Fingerprint::of(&path) {
            self.journal.record(OpRecord::Create {
                path: path.clone(),
                is_dir: false,
                fingerprint,
                created_parents: Vec::new(),
            });
        }
        self.toasts.undo(
            format!(
                "Pasted {} into {name}",
                crate::format::human_size(bytes.len() as u64)
            ),
            now,
        );
        let cwd = self.cwd();
        self.rescan(&cwd, now);
        self.dir().cursor_to_name(&name);
    }

    // ── The frame ───────────────────────────────────────────────────────────

    /// Everything this frame draws. One `&mut Ui` covering the window; painting
    /// is done through the painter rather than egui widgets, because the whole
    /// visual language (PLAN §8) is hand-drawn — rows, ripples, scrims — and
    /// mixing in a widget theme would only be a second set of rules to fight.
    fn frame(&mut self, ui: &mut egui::Ui) {
        let now = Instant::now();
        let area = ui.max_rect();
        let painter = ui.painter().clone();
        painter.rect_filled(area, 0, self.palette.crust);

        // Layout first: a page is however many rows fit, so the keys cannot be
        // routed until the panes have been measured. It is measured *again*
        // afterwards, because `t` and `Ctrl+c` change whether there is a tab
        // strip and therefore how tall the panes are — painting this frame with
        // the pre-keystroke geometry would leave the strip a frame behind the
        // key that asked for it, on a frame nothing would follow.
        let layout = ui::layout(area, self.mgr.ratio, self.tabs.len() > 1);
        // How the list pane is drawn, published to the two things that run
        // *before* the pane is measured: the cursor commands and the wheel.
        let first_metrics = (self.view_of(self.tab().cwd.path()) == View::Grid)
            .then(|| grid::metrics(ui::content_rect(layout.list).width()));
        self.columns = first_metrics.as_ref().map(|m| m.columns).unwrap_or(1);
        self.pane_step = grid::pane_step(first_metrics.as_ref());
        let page =
            crate::viewport::visible_rows(ui::content_rect(layout.list).height(), self.pane_step);
        self.route_keys(page, now);
        self.which.update(self.keys.which_key_due(), now);
        // The two write-behind timers, both of which are deadlines rather than
        // polls: the search's debounce and the state file's.
        if let Some(search) = &mut self.search {
            search.tick(now);
            // PLAN §7.2's "missing fd/rg → notice". The panel's own status line
            // already says it, but the panel is where you are *not* looking
            // when nothing appears in it, so the failure is also raised where
            // every other failure in this program is.
            if let Some(message) = search.take_notice() {
                self.toasts.error(message, now);
            }
        }
        self.tick_state(now);

        let layout = ui::layout(area, self.mgr.ratio, self.tabs.len() > 1);
        let list_content = ui::content_rect(layout.list);
        // Which geometry this directory is drawn in, decided once and threaded
        // everywhere through `grid::pane_*` (PLAN §2). `None` is the list.
        let metrics = (self.view_of(self.tab().cwd.path()) == View::Grid)
            .then(|| grid::metrics(list_content.width()));
        // A "page" is a *row* of the pane either way — for a grid that is a
        // whole row of tiles, so `Ctrl+d` moves the same distance down the
        // window in both views.
        let page =
            crate::viewport::visible_rows(list_content.height(), grid::pane_step(metrics.as_ref()));

        // ── Pointer (PLAN §7.5) ─────────────────────────────────────────────
        let pointer = ui.input(|i| Pointer {
            at: i.pointer.interact_pos(),
            down: i.pointer.primary_down(),
            pressed: i.pointer.primary_pressed(),
            released: i.pointer.primary_released(),
            secondary: i.pointer.secondary_pressed(),
            middle: i.pointer.button_pressed(egui::PointerButton::Middle),
            // The events themselves, not egui's smoothed delta: that one is
            // meant for widgets egui is animating, and these panes run their
            // own momentum (see [`crate::mouse::Fling`]).
            wheel: i
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::MouseWheel { unit, delta, .. } => {
                        Some(crate::mouse::wheel_points(wheel_unit(*unit), delta.y))
                    }
                    _ => None,
                })
                .sum::<f32>(),
            shift: i.modifiers.shift,
            toggle: i.modifiers.command || i.modifiers.ctrl,
            alt: i.modifiers.alt,
        });
        let scroll_rows = self.tab().cwd.scroll_rows(now);
        let parent_content = ui::content_rect(layout.parent);
        let parent_page = crate::viewport::visible_rows(parent_content.height(), ui::ROW_HEIGHT);
        let parent_scroll = self
            .tab()
            .parent
            .as_ref()
            .map(|parent| parent.scroll_rows(now))
            .unwrap_or(0.0);
        let parent_len = self.tab().parent.as_ref().map(|p| p.dir.len()).unwrap_or(0);
        let slide = self.tabs.offset(now);
        let tab_count = self.tabs.len();
        let overlay = self.overlay_geometry(area, &layout, layout.bar.top());

        // The breadcrumb is measured once and used by both the hit test and the
        // paint, for the reason `tab_rects` is: two functions computing this
        // separately is how a bar grows a one-pixel lie at its edges.
        self.sync_path_bar();
        let branch_room = chrome::branch_width(&painter, self.path_bar.2.as_deref());
        let crumb_rects = chrome::crumb_rects(&painter, layout.path, &self.path_bar.1, branch_room);
        let menu_geometry = self
            .menu
            .as_ref()
            .map(|menu| menu::geometry(area, menu, &painter));
        // A menu that is fading is pixels, not a surface: it takes no pointer.
        let menu_live = self.menu.as_ref().is_some_and(Menu::live);

        // The basket tray (PLAN §7.1), measured before the hit test for the
        // reason the breadcrumb is: two functions working it out separately is
        // how a floating surface grows a one-pixel lie at its edges.
        let basket_geometry =
            crate::basket::geometry(area, &self.basket, self.basket_open, self.basket_first);

        let over = pointer.at.and_then(|p| {
            // The menu is over everything, a modal card included: it is the
            // most recent thing the user asked for.
            if menu_live {
                return menu_geometry
                    .as_ref()
                    .and_then(|g| g.hit(p))
                    .map(|control| (control, p));
            }
            // A modal surface takes the pointer with the keyboard: nothing
            // behind the scrim is hoverable, so a stray click cannot move the
            // cursor under a question about the row it was on.
            if let Some(overlay) = &overlay {
                return overlay.hit(p).map(|control| (control, p));
            }
            // The tray floats over the panes, so it is hit-tested before them:
            // a click on the chip must not also land on the row underneath it.
            if basket_geometry.contains(p) {
                let control = basket_geometry
                    .remove_at(p)
                    .map(Control::BasketRemove)
                    .or_else(|| basket_geometry.row_at(p).map(Control::BasketRow))
                    .or_else(|| {
                        basket_geometry
                            .chip
                            .contains(p)
                            .then_some(Control::BasketChip)
                    })?;
                return Some((control, p));
            }
            let control = layout
                .strip
                .and_then(|strip| chrome::tab_at(strip, tab_count, p))
                .map(Control::Tab)
                .or_else(|| {
                    crumb_rects
                        .iter()
                        .position(|rect| rect.contains(p))
                        .map(Control::Crumb)
                })
                .or_else(|| {
                    grid::pane_at(
                        list_content,
                        metrics.as_ref(),
                        scroll_rows,
                        self.tab().cwd.dir.len(),
                        p,
                    )
                    .map(|index| Control::Row(Column::List, index))
                })
                .or_else(|| {
                    // The parent column is clickable too (PLAN §7.5): a click
                    // on it is "go there", which is what the column is showing.
                    ui::row_at(parent_content, parent_scroll, parent_len, p)
                        .map(|index| Control::Row(Column::Parent, index))
                })?;
            Some((control, p))
        });

        // The menu follows the pointer: hovering a row makes it the keyboard's
        // row too (one cursor, not two), and hovering the chevron flies the
        // submenu out — which is what a menu does everywhere and the reason
        // nobody has to be told a submenu is there.
        if menu_live {
            if let Some((Control::MenuItem(index), _)) = over {
                let submenu = self
                    .menu
                    .as_ref()
                    .and_then(|menu| menu.items.get(index))
                    .filter(|item| item.enabled)
                    .map(|item| item.submenu());
                if let (Some(submenu), Some(menu)) = (submenu, &mut self.menu) {
                    menu.cursor = Some(index);
                    if submenu {
                        menu.open_submenu();
                    } else {
                        // Moving off the parent row puts the submenu away. The
                        // cards overlap by `SUBMENU_OVERLAP` so the diagonal
                        // travel from the parent row into the submenu never
                        // passes over another row on the way.
                        menu.close_submenu();
                    }
                }
            }
        }

        // ── The wheel, with momentum (PLAN §7.5, §8) ────────────────────────
        // Routed by what the pointer is *over*, not by what has focus: a wheel
        // is aimed with the hand, and scrolling the pane the keyboard happens
        // to be in would be the one control in the program that ignores where
        // it was pointed.
        if pointer.wheel != 0.0 {
            if let Some(at) = pointer.at {
                self.wheel(pointer.wheel, at, &layout, page, parent_page, now);
            }
        }

        // ── Press ───────────────────────────────────────────────────────────
        // **Mousedown-capture focuses a pane** (PLAN §2.1). Before the click
        // itself, and on the pane rather than on anything in it: clicking the
        // empty space under a listing is still a claim about where you want the
        // keyboard, and a click that focused only when it landed on a row would
        // be a rule nobody could see.
        let any_press = pointer.pressed || pointer.secondary || pointer.middle;
        if let (Some(position), true) = (pointer.at, any_press) {
            if overlay.is_none() && !menu_live {
                if layout.list.contains(position) {
                    self.focus = Focus::List;
                } else if layout.preview.contains(position) {
                    self.focus = Focus::Preview;
                } else if layout.parent.contains(position) {
                    self.focus = Focus::Parent;
                }
            }
        }

        // A press anywhere but on the menu dismisses it, and the press is spent
        // doing so: a click that closed a menu *and* moved the cursor under it
        // would act on something the menu was covering.
        let dismissing = menu_live
            && any_press
            && !pointer
                .at
                .zip(menu_geometry.as_ref())
                .is_some_and(|(p, g)| g.contains(p));
        if dismissing {
            self.close_menu(now);
        }

        let geom = Geom {
            layout: &layout,
            page,
            list: list_content,
            list_scroll: scroll_rows,
            grid: metrics,
            parent: parent_content,
            parent_scroll,
            crumbs: &crumb_rects,
            overlay: &overlay,
            menu: &menu_geometry,
            basket: &basket_geometry,
            tabs: tab_count,
        };

        if pointer.secondary && !dismissing && overlay.is_none() && !menu_live {
            if let Some(position) = pointer.at {
                self.right_click(position, over.map(|(control, _)| control), &layout);
            }
        }

        if let Some((control, position)) = over.filter(|_| pointer.pressed && !dismissing) {
            // Everything happens on mouse-*down*, with the ripple: waiting for
            // the release would put the acknowledgement after the thing it is
            // acknowledging.
            let double = self.clicks.press(control, position, now);
            let rect = self.click(control, double, &pointer, &geom, now);
            self.ripples.spawn(control, position, rect, now);
        }

        // Middle click: a new tab on the row it landed on (PLAN §7.5).
        if pointer.middle && !menu_live && overlay.is_none() {
            if let Some((control, _)) = over {
                self.middle_click(control, now);
            }
        }

        // ── Drag: the band, and the seam the DnD phase takes ────────────────
        // Not while the menu owns the pointer: a drag that began *on* a menu
        // row is a slip of the hand, not a band select of the rows underneath.
        if pointer.pressed && !dismissing && !menu_live {
            self.press = pointer.at.map(|at| PressStart {
                at,
                on_row: matches!(over, Some((Control::Row(Column::List, _), _))),
                on_basket: matches!(over, Some((Control::BasketChip, _))),
                on_tab: match over {
                    Some((Control::Tab(index), _)) => Some(index),
                    _ => None,
                },
                in_list: layout.list.contains(at) && overlay.is_none(),
                dragging: false,
            });
        }
        if pointer.released {
            // A tab in the hand is decided *here*, unlike a file drag: there is
            // no drop target to resolve, only the question of whether the chip
            // was really pulled out of the strip.
            if self.tab_drag.is_some() {
                if let (Some(at), Some(strip)) = (pointer.at, layout.strip) {
                    self.release_tab_drag(at, strip, now);
                } else if let Some(drag) = self.tab_drag.take() {
                    // No strip to measure against — the last tab closed under
                    // the gesture. Nothing happens, visibly.
                    let at = drag.at;
                    self.spring_tab_home(drag, at, now);
                }
            }
            // …but *not* the file drag: `tick_drag` reads the release as the
            // drop, and clearing the press here only stops the gesture
            // re-arming.
            self.press = None;
            // The band commits as it goes, so releasing is only letting go.
            self.band = None;
        }
        if pointer.down {
            self.drag(
                pointer.at,
                list_content,
                layout.strip,
                scroll_rows,
                metrics,
                now,
            );
        }

        // ── Drag and drop (PLAN §7.1) ───────────────────────────────────────
        // The geometry is built whether or not anything is being dragged: a
        // drop from another application arrives between frames, and it is
        // resolved against the frame the hand was aiming at.
        let zones = dnd::Zones {
            strip: layout.strip,
            tabs: tab_count,
            crumbs: crumb_rects.clone(),
            list_pane: layout.list,
            list_content,
            list_scroll: scroll_rows,
            list_grid: metrics,
            list_rows: self.tab().cwd.dir.len(),
            parent_pane: layout.parent,
            parent_content,
            parent_scroll,
            parent_rows: parent_len,
        };
        self.poll_data_device(now);
        let dragging = self.tick_drag(&zones, &pointer, area, (page, parent_page), now);
        // The external drag's own highlight follows the pointer exactly as the
        // internal one's does — `wl_data_device` reports surface-local motion,
        // so a drop from another application is aimed, not guessed.
        let incoming_target = self
            .incoming
            .as_ref()
            .map(|incoming| incoming.at)
            .and_then(|at| {
                let tab = self.tab();
                dnd::target_at(&zones, at, |column, index| match column {
                    Column::List => tab.cwd.dir.row(index).is_some_and(|e| e.is_dir()),
                    Column::Parent => tab
                        .parent
                        .as_ref()
                        .and_then(|p| p.dir.row(index))
                        .is_some_and(|e| e.is_dir()),
                })
            });
        self.targets.tick(
            dragging
                .as_ref()
                .and_then(|frame| frame.target)
                .or(incoming_target),
            None,
            now,
        );
        self.zones = Some(zones);
        // A spring-back that has landed is over: an `Option` that is never
        // `None` is a window that never stops asking for frames (PLAN §1).
        if self
            .spring_back
            .as_ref()
            .is_some_and(|home| home.spring.finished(now))
        {
            self.spring_back = None;
        }
        // The rows in the hand are dimmed while they are in it — the same
        // treatment a cut row gets, and for the same reason: it is on its way
        // out of this listing.
        let drag_paths: HashSet<PathBuf> = self
            .drag
            .as_ref()
            .map(|drag| drag.paths.iter().cloned().collect())
            .unwrap_or_default();

        self.hovers.tick(
            over.map(|(control, _)| control),
            over.map(|(control, _)| control).filter(|_| pointer.down),
            now,
        );
        self.ripples.tick(now);

        // `delightful-ui` §2: the pointer says what is clickable. A file row
        // keeps the arrow — it is a place, and a hand over every row of a
        // thousand-row listing is noise — while everything that is a *button*
        // says so.
        // A live drag says so with the cursor before it says so with anything
        // else (`delightful-ui` §2), and it overrides whatever is under it —
        // the hand is holding files, not pointing at a link.
        if dragging.is_some() || self.tab_drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if let Some((control, _)) = over {
            ui.ctx().set_cursor_icon(match control {
                Control::Row(..) => egui::CursorIcon::Default,
                // A tab chip is draggable as well as clickable — it is how a
                // tab becomes a window (PLAN §2) — so it wears the hand that
                // says so (`delightful-ui` §2), like the basket's chip.
                Control::Tab(_) | Control::BasketChip => egui::CursorIcon::Grab,
                Control::Crumb(_)
                | Control::Action(_)
                | Control::PanelRow(_)
                | Control::MenuItem(_)
                | Control::SubmenuItem(_)
                | Control::BasketRow(_)
                | Control::BasketRemove(_) => egui::CursorIcon::PointingHand,
            });
        }

        // ── Scroll ──────────────────────────────────────────────────────────
        // The scrolloff rule is applied to the *target* row, not to where the
        // rows have animated to, so the maths never chases its own animation.
        let scrolloff = self.mgr.scrolloff;
        let tab = self.tabs.active_mut();
        // Counted in *rows of the pane*: one entry per row in the list, a
        // whole row of tiles in the grid. With one column this is the
        // arithmetic the list always had, to the digit.
        let columns = metrics.as_ref().map(|m| m.columns).unwrap_or(1);
        let pane_rows = match &metrics {
            Some(metrics) => metrics.rows(tab.cwd.dir.len()),
            None => tab.cwd.dir.len(),
        };
        let list_first = crate::viewport::first_visible(
            tab.cwd.first(),
            tab.cwd.dir.cursor() / columns,
            pane_rows,
            page,
            scrolloff,
        );
        tab.cwd.set_first(list_first, now);
        let cursor = tab.cwd.dir.cursor();
        self.cursor_glow.tick(Some(cursor), None, now);
        // Focus commits instantly; its picture catches up (PLAN §2.1).
        self.focus_fade.tick(self.focus, now);
        // Where a rename popup and the opener picker anchor themselves — the
        // row the cursor is on, as it was actually drawn this frame.
        self.cursor_rect = grid::pane_rect(list_content, metrics.as_ref(), scroll_rows, cursor);

        if let Some(parent) = &mut self.tabs.active_mut().parent {
            let first = crate::viewport::first_visible(
                parent.first(),
                parent.dir.cursor(),
                parent.dir.len(),
                parent_page,
                scrolloff,
            );
            parent.set_first(first, now);
        }

        // ── The preview (PLAN §6) ───────────────────────────────────────────
        // Asked for here, *after* the keys have been routed: the request is for
        // where the cursor ended up, not for each row it passed through. The
        // debounce in df-core does the rest.
        let ppp = ui.ctx().pixels_per_point();
        let preview_content = ui::content_rect(layout.preview);
        let target = (
            (preview_content.width() * ppp).max(0.0) as u32,
            (preview_content.height() * ppp).max(0.0) as u32,
        );
        // PLAN §7.2's live preview: while the search panel is open, the pane
        // shows the *highlighted result*, not the row the cursor happens to be
        // parked on behind it. That is what makes the panel a search and not a
        // list of paths.
        let searched = self
            .search
            .as_ref()
            .and_then(Search::chosen)
            .map(|hit| (hit.path.clone(), hit.line));
        let hovered = match &searched {
            Some((path, _)) => Some(path.clone()),
            None => self
                .tab()
                .cwd
                .dir
                .cursor_entry()
                .map(|entry| entry.path.clone()),
        };
        // Inside an archive the hovered path is a fiction, so the preview
        // workers are given nothing at all and the pane draws the entry's card
        // instead (see `paint_archive_card`). Handing them a path that is not
        // on the disk would be a failed read per cursor move.
        // The trash is deliberately *not* on this list: its rows are real
        // files inside `…/Trash/files/`, so the ordinary preview pipeline
        // opens them and a trashed photo looks like a photo (PLAN §7.4).
        let in_archive = self.tab().archive.is_some() || self.tab().remote.is_some();
        if in_archive {
            self.preview.sync(None, target, now);
        } else {
            self.preview.sync(hovered.as_deref(), target, now);
        }
        // …and a content hit scrolls the pane to the line it matched on. Only
        // when something changed — the cursor moved, or the preview finished
        // loading — so a settled panel is not re-scrolling the pane sixty times
        // a second and holding the window awake.
        if let (true, Some((_, Some(line)))) = (self.search_follow, &searched) {
            self.preview.scroll_to(line.saturating_sub(1), now);
            self.search_follow = false;
        }
        // The document worker's two inputs, both of which live on this side of
        // the seam: the four colours a rasteriser may draw with, and whether
        // this pane has the keyboard — which is the turntable's whole switch
        // (PLAN §1: idle discipline beats spin).
        self.preview
            .set_ink(crate::preview::doc::Ink::from_palette(&self.palette));
        self.preview.set_focused(self.focus == Focus::Preview);
        self.preview.sync_doc(now);
        // The wheel's coast over the document, sampled once a frame (PLAN §7.5).
        self.preview.tick_fling(now);
        // The transport follows the same cursor, one line later and by the same
        // rule: after the keys, so a held `↓` mounts what it stopped on.
        self.sync_playback(now);
        // What the player has open, and the frame it is showing. Both are read
        // here — before the painter's borrow of the palette — because the frame
        // conversion needs `&mut self.gfx` and the paint needs `&self`.
        let mounted = self.transport().is_some();
        self.preview.set_media_mounted(mounted);
        // What the strip draws, with the playhead interpolated between decoded
        // frames so the bar glides rather than hopping one frame at a time.
        let transport = self
            .player
            .as_mut()
            .filter(|_| mounted)
            .map(|player| player.state_at(now));
        // The loop's wrap is asked once per frame while `L` is on — it is an
        // anticipation of the end, not a reaction to it (see `wrap_if_ending`).
        if let Some(player) = self.player.as_mut() {
            player.wrap_if_ending(now);
        }
        // The decoded frame, converted into an egui texture. One pass per
        // *decoded* frame, not per painted one: a paused video re-uses the
        // texture it already has.
        let frame_tex = match (self.gfx.as_mut(), self.player.as_mut()) {
            (Some(gfx), Some(player)) if mounted => {
                let (device, queue) = (gfx.device.clone(), gfx.queue.clone());
                player.frame(&device, &queue, &mut gfx.renderer)
            }
            _ => None,
        };
        let strip_alpha = self
            .player
            .as_ref()
            .filter(|_| mounted)
            .map(|player| player.strip_alpha(now))
            .unwrap_or(0.0);
        let media_info = self
            .player
            .as_ref()
            .filter(|_| mounted)
            .and_then(|player| player.info().cloned());

        // ── The help sheet, and where it has scrolled to ─────────────────────
        // Built before the painter exists, because building it needs `&mut
        // self` and the painter holds the palette.
        let help_view = match self.help {
            Some(mut help) => {
                let rect = chrome::help_rect(area, layout.bar);
                let lines = self.help_lines();
                let total = help::all_rows(&self.keymap, &self.help_stack(), WhenFlags::LIST).len();
                // The same scrolloff rule the panes use, on the same numbers:
                // one list-scrolling behaviour in the program, not two.
                help.first = crate::viewport::first_visible(
                    help.first,
                    help.cursor,
                    lines.len(),
                    chrome::help_page(rect),
                    scrolloff,
                );
                self.help = Some(help);
                Some((rect, lines, total, help))
            }
            None => None,
        };

        // The two new overlays' scroll, by the same scrolloff rule the panes
        // use — one list-scrolling behaviour in the program, not three.
        if let Some(finder) = &mut self.finder {
            finder.scroll_into_view(scrolloff);
        }
        if let (Some(search), Some(OverlayGeom::Search(geometry))) = (&mut self.search, &overlay) {
            search.scroll_into_view(geometry.page(), scrolloff);
        }

        // ── The clock-driven bits, ticked once, before anything is drawn ────
        self.toasts.tick(now);
        // The rows the `w` panel is about, and the bars' targets. Built here
        // because the panel is painted from the same list the pointer was hit
        // tested against.
        let task_rows = match &overlay {
            Some(OverlayGeom::Panel(_, _, rows)) => rows.clone(),
            _ => Vec::new(),
        };
        if let Some(panel) = &mut self.panel {
            panel.tick(&task_rows, now);
        }
        // What the clipboard is holding, as a set the row painter can ask in
        // constant time.
        let clip_paths: HashSet<PathBuf> = self.clipboard.paths.iter().cloned().collect();

        // ── The FLIP's "Last" (PLAN §2, §8) ─────────────────────────────────
        // Where every item the pane is about to draw *is*. Captured before the
        // painter exists because building it needs `&self` and the painter
        // holds the palette, and captured every frame because it is also the
        // "First" of whatever re-sort happens next.
        let layout_now = self.pane_layout(list_content, metrics.as_ref(), scroll_rows);
        if let Some(before) = self.flip_before.take() {
            self.flip = Flip::begin(&before, &layout_now, now);
        }
        self.last_layout = layout_now;
        // A finished animation is dropped rather than left holding a `Some`:
        // an option that is never `None` is a window that never stops asking
        // for frames (PLAN §1).
        if self.flip.as_ref().is_some_and(|flip| flip.finished(now)) {
            self.flip = None;
        }

        // ── The grid's thumbnails (PLAN §2) ─────────────────────────────────
        // Asked for before the painter, for the same borrow reason, and only
        // for the tiles that are on screen or one row past it — the cancel on
        // scroll past is `Thumbs::want` replacing the queue outright.
        if let Some(metrics) = &metrics {
            let wants = self.tile_wants(list_content, metrics, scroll_rows);
            self.thumbs().want(&wants);
        }

        // What git thinks of this directory (PLAN §7.3), asked once for the
        // whole frame. An `Arc` clone out of the cache: the rows then read it
        // without touching the lock the worker swaps statuses in under, so a
        // status landing mid-frame cannot change the picture half way down a
        // pane.
        let repo_status = self.repo_status();
        let repo_status = repo_status.as_deref();
        // The du mode is only ever about the directory that is on screen, and
        // it is ended by navigating away — this check is the belt to that
        // brace, for the frame between a navigation and the next update.
        let cwd_now = self.cwd();

        // ── Paint ───────────────────────────────────────────────────────────
        let paint = ui::Painting {
            painter: &painter,
            palette: &self.palette,
            theme: &self.theme,
            nerd: self.nerd,
            show_symlink: self.mgr.show_symlink,
            now,
        };

        // PLAN §2.1's focus visuals: the pane with the keyboard wears the 2 px
        // accent rule and the 4% tint, and exactly one pane ever does.
        // …as an *amount*, so the treatment eases across instead of popping
        // (PLAN §2.1's 120 ms). The amounts always sum to 1: one pane is
        // arriving at exactly the rate the other is leaving.
        let parent_focus = self.focus_fade.amount(Focus::Parent, now);
        let list_focus = self.focus_fade.amount(Focus::List, now);
        let preview_focus = self.focus_fade.amount(Focus::Preview, now);
        let list_ground = paint.pane_fill(self.palette.base, list_focus);
        paint.pane(layout.parent, self.palette.mantle, parent_focus);
        paint.pane(layout.list, self.palette.base, list_focus);
        paint.pane(layout.preview, self.palette.mantle, preview_focus);

        if let Some(parent) = &self.tab().parent {
            paint.listing(ListView {
                pane: layout.parent,
                ground: self.palette.mantle,
                dir: &parent.dir,
                scroll_rows: parent.scroll_rows(now),
                column: Column::Parent,
                hovers: &self.hovers,
                ripples: &self.ripples,
                cursor_fill: self.palette.surface0,
                cursor_glow: CursorGlow::Steady,
                // The parent's marker is normally a fact about the path rather
                // than a cursor, so it stays quiet — until the keyboard is
                // actually in that pane and it *is* the cursor.
                cursor_alpha: crate::ui::ghost_cursor(parent_focus),
                linemode: LineMode::None,
                dim: true,
                slow_load: now.duration_since(parent.scan_started) >= LOADING_DELAY,
                offset_x: slide,
                show_selection: false,
                // The clipboard's marks belong to the directory the yank was
                // made in, which is the list — the parent shows where you are,
                // not what you are carrying.
                clip: None,
                dragged: &drag_paths,
                // The parent column never re-sorts on its own — it follows the
                // list's sort, and by then it is a *different* listing.
                flip: None,
                // The same repository the list is in: the parent is an ancestor
                // of the current directory, so either it is inside the same
                // work tree — where the repository root's own row wants its
                // rollup dot — or it is above it, where every lookup misses and
                // the column is drawn empty. Both are right.
                git: repo_status,
                // Never in the parent column: the mode is about the directory
                // you are in, and a bar there would be measuring a different
                // parent's children.
                usage: None,
                // …and neither is the trash's column: the parent beside the
                // trash is a real directory, whose rows want their linemode.
                notes: None,
            });
        }
        let list_view = ListView {
            pane: layout.list,
            ground: list_ground,
            dir: &self.tab().cwd.dir,
            scroll_rows,
            column: Column::List,
            hovers: &self.hovers,
            ripples: &self.ripples,
            cursor_fill: self.palette.surface1,
            cursor_glow: CursorGlow::Fading(&self.cursor_glow),
            // DelightMail's vim-split trick (PLAN §2.1): with the keyboard
            // somewhere else the cursor row dims to a ghost bar, so "where am
            // I" and "where do my keys go" are two questions with two answers
            // and both are always on screen.
            cursor_alpha: crate::ui::ghost_cursor(list_focus),
            linemode: self.mgr.linemode,
            dim: false,
            slow_load: now.duration_since(self.tab().cwd.scan_started) >= LOADING_DELAY,
            offset_x: slide,
            show_selection: true,
            clip: (!clip_paths.is_empty()).then(|| ClipMark {
                paths: &clip_paths,
                cut: self.clipboard.mode == PasteMode::Cut,
            }),
            dragged: &drag_paths,
            flip: self.flip.as_ref(),
            git: repo_status,
            usage: self.usage.as_ref().filter(|u| u.is_about(&cwd_now)),
            // PLAN §7.4: in the trash the column is where each row came from,
            // which is the fact the view is read for.
            notes: self.tab().trash.is_some().then_some(&self.trash_notes),
        };
        match (&metrics, &self.thumbs) {
            // PLAN §2's grid. Same directory, same cursor, same selection and
            // the same drag — a different function from an index to a
            // rectangle, and nothing else.
            (Some(metrics), Some(thumbs)) => grid::paint(
                &paint,
                GridView {
                    pane: list_view.pane,
                    ground: list_view.ground,
                    dir: list_view.dir,
                    scroll_rows: list_view.scroll_rows,
                    metrics: *metrics,
                    hovers: list_view.hovers,
                    ripples: list_view.ripples,
                    cursor_glow: &self.cursor_glow,
                    cursor_alpha: list_view.cursor_alpha,
                    thumbs,
                    clip: list_view.clip,
                    dragged: list_view.dragged,
                    flip: list_view.flip,
                    slow_load: list_view.slow_load,
                },
            ),
            // The list, and the impossible case where a grid is wanted but its
            // workers would not start — which is a directory drawn as a list
            // rather than a directory drawn as nothing.
            _ => paint.listing(list_view),
        }
        // The band, over the rows it is selecting (PLAN §7.5). A wash and a
        // hairline: it has to be unmistakable without hiding the names it is
        // being drawn across, so the fill is barely there and the *edge* is
        // what makes it a rectangle.
        if let (Some(band), Some(at)) = (&self.band, pointer.at) {
            let rect = crate::mouse::band(band.origin, at).intersect(list_content);
            let clipped = painter.with_clip_rect(list_content);
            clipped.rect_filled(
                rect,
                ui::ROW_RADIUS,
                chrome::fade(self.palette.blue, BAND_FILL),
            );
            clipped.rect_stroke(
                rect,
                ui::ROW_RADIUS,
                egui::Stroke::new(1.0, chrome::fade(self.palette.blue, BAND_EDGE)),
                egui::StrokeKind::Inside,
            );
        }
        // Inside an archive the preview pane is the entry's facts card
        // (PLAN §7.3), because there is no file on the disk for the preview
        // pipeline to open.
        match self.tab().archive.as_ref() {
            Some(browse) => {
                let entry = self
                    .tab()
                    .cwd
                    .dir
                    .cursor_entry()
                    .and_then(|row| browse.inner(&row.path))
                    .and_then(|inner| browse.tree.get(&inner));
                match entry {
                    Some(entry) => {
                        let body = self
                            .archive_preview
                            .as_ref()
                            .filter(|(a, i, _)| *a == browse.path && *i == entry.path)
                            .and_then(|(_, _, body)| body.as_deref());
                        crate::archive::card(
                            &paint,
                            layout.preview,
                            entry,
                            browse.tree.format(),
                            body,
                        );
                    }
                    // An empty archive, or a filter that matched nothing: the
                    // same quiet label an empty directory's preview gets.
                    None => paint.quiet_label(ui::content_rect(layout.preview), "nothing to show"),
                }
            }
            // On a remote service the hovered path is a URL, so the pane is the
            // row's facts card and — for small text that has come down — its
            // body (PLAN §7.6).
            None if self.tab().remote.is_some() => {
                let service = self
                    .tab()
                    .remote
                    .as_ref()
                    .map(|s| s.at.service.clone())
                    .unwrap_or_default();
                match self.tab().cwd.dir.cursor_entry() {
                    Some(entry) => {
                        let state = self
                            .remote_preview
                            .as_ref()
                            .filter(|p| Path::new(&p.url) == entry.path);
                        crate::remote::card(
                            &paint,
                            layout.preview,
                            entry,
                            &service,
                            state.and_then(|p| p.body.as_deref()),
                            state.is_some_and(|p| p.loading),
                        );
                    }
                    None => paint.quiet_label(ui::content_rect(layout.preview), "nothing to show"),
                }
            }
            None => crate::preview::preview(&paint, layout.preview, &mut self.preview, ppp, now),
        }
        // Over the pane's own body — the cached thumbnail is the poster the
        // first decoded frame lands on top of — and under everything else.
        if let Some(state) = &transport {
            let content = ui::content_rect(layout.preview);
            let clipped = painter.with_clip_rect(content);
            match frame_tex {
                Some(tex) => {
                    let rect = crate::preview::fit_rect(content, (tex.width, tex.height), ppp);
                    let mut mesh = egui::Mesh::with_texture(tex.id);
                    mesh.add_rect_with_uv(
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    clipped.add(egui::Shape::mesh(mesh));
                }
                // Audio, or a video whose first frame has not landed: the card
                // says what the list cannot — how long, what codec, what rate.
                None => {
                    if let Some(info) = &media_info {
                        if !info.has_video {
                            crate::playback::strip::audio_card(&paint, content, info, 1.0);
                        }
                    }
                }
            }
            crate::playback::strip::paint(&paint, content, state, strip_alpha);
        }

        // ── The chrome ──────────────────────────────────────────────────────
        if let Some(strip) = layout.strip {
            let titles: Vec<String> = self.tabs.iter().map(Tab::title).collect();
            chrome::tab_strip(
                &paint,
                strip,
                &titles,
                self.tabs.active_index(),
                &self.hovers,
                &self.ripples,
            );
        }
        // The basket tray, over the panes and under every modal (PLAN §7.1).
        crate::basket::paint(
            &paint,
            &self.basket,
            &basket_geometry,
            self.basket_first,
            &self.hovers,
            &self.ripples,
        );
        chrome::path_bar(
            &paint,
            layout.path,
            &self.path_bar.1,
            &crumb_rects,
            self.path_bar.2.as_deref(),
            &self.hovers,
            &self.ripples,
        );

        // The help sheet is drawn over the panes but *under* the bar, because
        // the bar is where its filter is typed — an overlay that covered its own
        // input would be asking a question it hid the answer box for.
        if let Some((rect, lines, total, help)) = &help_view {
            chrome::help_overlay(&paint, area, *rect, lines, help, *total);
        }

        // An anchored prompt (`r`, `R`, the conflict rename) floats over the row
        // it is about, so the bar keeps saying where you are underneath it.
        let anchored = self.prompt.as_ref().filter(|prompt| prompt.kind.anchored());
        match &self.prompt {
            Some(prompt) if prompt.kind.anchored() => {
                let dir = &self.tab().cwd.dir;
                chrome::status_bar(
                    &paint,
                    layout.bar,
                    chrome::Status {
                        selected: dir.selected_count(),
                        position: if dir.is_empty() { 0 } else { dir.cursor() + 1 },
                        rows: dir.len(),
                        filter: dir.filter(),
                        visual: self.visual.as_ref().map(|v| v.selecting),
                    },
                );
            }
            Some(prompt) => chrome::input_bar(&paint, layout.bar, prompt),
            None if self.finder.is_some() => chrome::hint_bar(
                &paint,
                layout.bar,
                &[("↑↓", "move"), ("Enter", "run"), ("Esc", "close")],
            ),
            None if self.search.is_some() => chrome::hint_bar(
                &paint,
                layout.bar,
                &[
                    ("↑↓", "move"),
                    ("Enter", "go there"),
                    ("Ctrl+s", "stop"),
                    ("Esc", "close"),
                ],
            ),
            None if self.overlay_open() => chrome::hint_bar(
                &paint,
                layout.bar,
                &overlay_hints(
                    &self.dialog,
                    self.picker.is_some(),
                    self.spot.is_some(),
                    self.mounts.is_some(),
                ),
            ),
            None if self.help.is_some() => chrome::hint_bar(
                &paint,
                layout.bar,
                &[("↑↓", "move"), ("f", "filter"), ("Esc", "close")],
            ),
            None => {
                let dir = &self.tab().cwd.dir;
                chrome::status_bar(
                    &paint,
                    layout.bar,
                    chrome::Status {
                        selected: dir.selected_count(),
                        position: if dir.is_empty() { 0 } else { dir.cursor() + 1 },
                        rows: dir.len(),
                        filter: dir.filter(),
                        visual: self.visual.as_ref().map(|v| v.selecting),
                    },
                );
            }
        }

        // ── The modal surfaces, over the panes and the bar ──────────────────
        match (&overlay, &self.dialog) {
            (Some(OverlayGeom::Mounts(geometry)), _) => {
                if let Some(card) = &self.mounts {
                    crate::mounts::paint(&paint, area, card, geometry, &self.hovers, &self.ripples);
                }
            }
            (Some(OverlayGeom::Bulk(geometry)), Some(Dialog::Bulk(bulk))) => {
                dialog::paint_bulk(&paint, area, bulk, geometry, &self.hovers, &self.ripples);
            }
            (Some(OverlayGeom::Confirm(geometry)), Some(Dialog::Confirm(confirm))) => {
                dialog::paint_confirm(&paint, area, confirm, geometry, &self.hovers, &self.ripples);
            }
            (Some(OverlayGeom::Conflict(geometry)), Some(Dialog::Conflict(conflict))) => {
                dialog::paint_conflict(
                    &paint,
                    area,
                    conflict,
                    geometry,
                    &self.hovers,
                    &self.ripples,
                );
            }
            _ => {}
        }
        if let (Some(OverlayGeom::Picker(card, rows)), Some(picker)) = (&overlay, &self.picker) {
            open::paint_picker(&paint, *card, rows, picker, &self.hovers, &self.ripples);
        }
        if let (Some(OverlayGeom::Spot(geometry)), Some(spot)) = (&overlay, &self.spot) {
            spot::paint(&paint, spot, geometry, &self.hovers, &self.ripples, now);
        }
        if let (Some(OverlayGeom::Finder(geometry)), Some(finder)) = (&overlay, &self.finder) {
            overlay::paint_finder(&paint, area, geometry, finder, &self.hovers, &self.ripples);
        }
        if let (Some(OverlayGeom::Search(geometry)), Some(search)) = (&overlay, &self.search) {
            overlay::paint_search(&paint, geometry, search, &self.hovers, &self.ripples);
        }
        if let (Some(OverlayGeom::Panel(card, rects, _)), Some(panel)) = (&overlay, &self.panel) {
            panel::paint(
                &paint,
                *card,
                rects,
                &task_rows,
                panel,
                &self.hovers,
                &self.ripples,
                now,
            );
        }
        // The floating prompt goes over the card that opened it — the conflict
        // resolver's rename is a field *in* that dialog.
        if let Some(prompt) = anchored {
            let anchor = match &overlay {
                Some(OverlayGeom::Conflict(geometry)) => geometry.card,
                _ => self.cursor_rect,
            };
            chrome::prompt_popup(&paint, area, anchor, prompt);
        }

        // ── The drag, over everything it is being carried across ────────────
        // After the chrome, because two of the four kinds of target *are* the
        // chrome: a ring drawn before the tab strip would be painted over by
        // the chip it is ringing.
        // A drag from another application, announced at the window's own edge.
        // Not for our *own* drag come back through the compositor: the window
        // does not need telling that it is about to be handed something it is
        // holding.
        if self
            .incoming
            .as_ref()
            .is_some_and(|incoming| !incoming.ours)
        {
            paint.drop_window(area);
        }
        if let Some(zones) = &self.zones {
            let progress = self
                .drag
                .as_ref()
                .map(|drag| drag.spring.progress(now))
                .unwrap_or(0.0);
            let live = dragging.as_ref().and_then(|frame| frame.target);
            for (target, amount) in self.targets.warm() {
                let Some(rect) = zones.rect_of(target) else {
                    continue;
                };
                let radius = match target {
                    dnd::Target::Pane(_) => ui::PANE_RADIUS,
                    _ => ui::ROW_RADIUS,
                };
                // The badge only fills on the target the hold is actually
                // against — a ring left fading behind the drag is a memory,
                // and a memory does not have a countdown running.
                let filling = if Some(target) == live { progress } else { 0.0 };
                paint.drop_target(rect, radius, amount, filling);
            }
        }
        if let (Some(frame), Some(drag)) = (&dragging, &self.drag) {
            let cards = dnd::ghost_cards(frame.at, drag.paths.len());
            paint.ghost(
                &cards,
                &ui::GhostFace {
                    icon: drag.icon,
                    name: &drag.label,
                    count: dnd::ghost_badge(drag.paths.len()),
                    verb: frame.verb.label(),
                },
                1.0,
            );
        }
        // A tab being pulled out of the strip (PLAN §2). One card, because a
        // tab is one thing; the verb chip says what letting go would do, and
        // it is empty until the gesture has actually armed — a ghost that
        // promised a window before the threshold was crossed would be the
        // gesture lying about itself.
        if let (Some(drag), Some(strip)) = (&self.tab_drag, layout.strip) {
            let armed = crate::window::armed(drag.from, drag.at, strip);
            let cards = dnd::ghost_cards(drag.at, 1);
            paint.ghost(
                &cards,
                &ui::GhostFace {
                    icon: drag.icon,
                    name: &drag.label,
                    count: None,
                    verb: if armed { "New window" } else { "" },
                },
                1.0,
            );
        }
        // …and the cancelled one on its way home, which outlives the drag.
        if let Some(home) = &self.spring_back {
            let cards = dnd::ghost_cards(home.spring.at(now), home.spring.count());
            paint.ghost(
                &cards,
                &ui::GhostFace {
                    icon: home.icon,
                    name: &home.label,
                    count: dnd::ghost_badge(home.spring.count()),
                    // A drag that is being cancelled is not carrying a verb any
                    // more; the chip goes with the decision it described.
                    verb: "",
                },
                home.spring.alpha(now),
            );
        }

        // The toast sits above the bar and under the which-key card: a message
        // about what just happened must not cover the answer to the key being
        // held down now.
        self.toasts.paint(&paint, area, layout.bar.top(), now);

        // The menu is over everything below it — it is the most recent thing
        // the user asked for — and under the which-key card, which is an answer
        // to a key being held down right now.
        if let (Some(menu), Some(geometry)) = (&self.menu, &menu_geometry) {
            menu::paint(&paint, menu, geometry, &self.hovers, &self.ripples, now);
        }

        // Last, and over everything: the card is an answer to a key that is
        // being held down right now, so nothing may cover it.
        if self.which.visible(now) {
            chrome::which_key(
                &paint,
                area,
                layout.bar.top(),
                &self.which_rows,
                self.which.alpha(now),
            );
        }

        // A menu whose fade is over is gone: see the `menu` row below.
        if self.menu.as_ref().is_some_and(|menu| menu.spent(now)) {
            self.menu = None;
        }

        // ── The repaint discipline, in one place (PLAN §1) ──────────────────
        // A frame is asked for only while something is actually moving. A
        // pointer parked on a row holds a 1.0 that will be 1.0 again next
        // frame, and `animating()` says so — idle costs zero frames.
        let animating = [
            ("hovers", self.hovers.animating()),
            // The focus treatment crossing between panes. It retires itself
            // once it has arrived (see `FocusFade::tick`), so a settled
            // keyboard costs nothing.
            ("focus", self.focus_fade.animating(now)),
            ("cursor_glow", self.cursor_glow.animating()),
            ("ripples", self.ripples.animating(now)),
            ("tab", self.tab().animating(now)),
            ("tabs", self.tabs.animating(now)),
            ("preview", self.preview.animating(now)),
            // The usage bars' grow-in, which stops asking the moment the sweep
            // has landed — and a walk that is still streaming does not ask
            // either, because a new number wakes the loop through the notifier
            // rather than by polling (PLAN §1).
            (
                "usage",
                self.usage.as_ref().is_some_and(|u| u.animating(now)),
            ),
            // A *playing* file streams frames and says so; a paused one says
            // so only while a frame it asked for — a seek, a step, a new
            // source — has not landed, and then stops (PLAN §1's idle rule).
            (
                "playback",
                self.player
                    .as_ref()
                    .is_some_and(|p| p.is_playing() || p.awaiting_frame()),
            ),
            ("which", self.which.fading()),
            // The menu's dismissal fade. It ends, and `Menu::spent` is what
            // drops it — an option that is never `None` is a window that never
            // stops asking for frames (PLAN §1).
            ("menu", self.menu.as_ref().is_some_and(|menu| !menu.live())),
            ("toast", self.toasts.animating(now)),
            // The FLIP's travel and the fades either side of it. It is dropped
            // the moment it arrives (see the frame), so this can never be
            // stuck on.
            ("flip", self.flip.is_some()),
            // The drop rings' fade-out, and the ghost's flight home. The drag
            // itself is not here: a ghost parked under a stationary pointer is
            // the same pixels next frame, and it moves only when the pointer
            // does — which brings its own frame with it.
            ("targets", self.targets.animating()),
            ("spring", self.spring_back.is_some()),
            (
                "tasks",
                self.panel.as_ref().is_some_and(|p| p.animating(now)),
            ),
        ];
        // DF_FRAME_LOG=1 names whoever is holding the frame rate up — the
        // instrument for the Phase 6 "zero repaints at rest" audit, because a
        // stuck `animating()` source is invisible from outside.
        if frame_log_enabled() {
            let hot: Vec<&str> = animating
                .iter()
                .filter(|(_, on)| *on)
                .map(|(n, _)| *n)
                .collect();
            log::info!("frame: animating={hot:?}");
        }
        if animating.iter().any(|(_, on)| *on) {
            ui.ctx().request_repaint();
        } else if let Some(due) = self.next_deadline(now) {
            // The *scheduled* wake-ups, and there are exactly two kinds: the
            // moment a slow read earns its label, and the moment a held chord
            // earns its which-key card. Both are a single instant known in
            // advance, so both are a `WaitUntil` and neither is a poll.
            ui.ctx().request_repaint_after(due);
        }
    }

    /// When the next frame is owed by something that is *waiting* rather than
    /// moving. `None` is the resting state: no deadline, no frame.
    fn next_deadline(&self, now: Instant) -> Option<Duration> {
        let card = self
            .which
            .deadline(self.keys.which_key_due())
            .map(|at| at.saturating_duration_since(now));
        // Four waiters now: a slow directory read, a held chord, the preview
        // (its own "reading…" label and its scrollbar's linger), and the toast
        // — which asks for exactly one wake-up, the instant its fade begins.
        // …and two of the transport's: the moment the position strip finishes
        // its linger and starts to fade, and the moment a source the cursor has
        // left runs out of its grace and is torn down. Both are single instants
        // known in advance, so both are a `WaitUntil` and neither is a poll.
        let strip = self
            .player
            .as_ref()
            .filter(|player| player.is_playing())
            .map(|player| player.strip_deadline().saturating_duration_since(now))
            .filter(|d| !d.is_zero());
        let grace = self
            .player
            .as_ref()
            .and_then(Player::grace_deadline)
            .map(|at| at.saturating_duration_since(now));
        // …and the drag's: a hold over a folder has to spring it open even if
        // the hand never moves again, which is a single instant known in
        // advance and therefore a deadline rather than a poll.
        let spring = self
            .drag
            .as_ref()
            .and_then(|drag| drag.spring.deadline())
            .map(|at| at.saturating_duration_since(now));
        // …the search's debounce, which is the instant a query stops changing
        // and a process is owed (PLAN §7.2)…
        let search = self.search.as_ref().and_then(|s| s.deadline(now));
        // …and the state file's write-behind (PLAN §2). Both are a single
        // instant known in advance, so both are a `WaitUntil` and neither is a
        // poll.
        let state = self.state_due.deadline(now);
        [
            self.loading_deadline(now),
            self.remote_preview_deadline(now),
            card,
            self.preview.next_deadline(now),
            self.toasts.deadline(now),
            strip,
            grace,
            spring,
            search,
            state,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// How long until a pane has to admit it is loading, if one is about to.
    fn loading_deadline(&self, now: Instant) -> Option<Duration> {
        let tab = self.tab();
        let panes = std::iter::once(&tab.cwd).chain(tab.parent.iter());
        panes
            .filter(|p| p.dir.is_empty() && p.dir.state() == df_core::fs::LoadState::Loading)
            .map(|p| (p.scan_started + LOADING_DELAY).saturating_duration_since(now))
            .filter(|d| !d.is_zero())
            .min()
    }

    fn redraw(&mut self) {
        self.watchdog.frame_started();
        self.redraw_inner();
        self.watchdog.frame_finished();
    }

    fn redraw_inner(&mut self) {
        self.repaint_at = None;
        // Drain first, so this frame already carries whatever the workers
        // finished while it was being asked for.
        self.poll_workers();
        let (raw_input, ctx) = {
            let Some(gfx) = &mut self.gfx else { return };
            (
                gfx.egui_state.take_egui_input(&gfx.window),
                gfx.egui_ctx.clone(),
            )
        };

        let mut full_output = ctx.run_ui(raw_input, |ui| self.frame(ui));

        let platform_output = std::mem::take(&mut full_output.platform_output);
        let Some(gfx) = &mut self.gfx else { return };
        gfx.egui_state
            .handle_platform_output(&gfx.window, platform_output);

        let repaint_delay = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|vp| vp.repaint_delay);

        if !gfx.present(full_output) {
            if frame_log_enabled() {
                log::info!("present-fail");
            }
            // Not an immediate re-request: a failed acquire has just cost up
            // to a second of wall-clock on this thread (see `Gfx::present`),
            // and asking again at once is a loop that blocks the UI for as
            // long as the compositor stays quiet. A short deadline lets input
            // and worker results through between attempts, and grows with
            // each consecutive failure so a window that is truly hidden costs
            // one probe every couple of seconds instead of one every frame.
            self.present_failures = self.present_failures.saturating_add(1);
            let backoff = PRESENT_RETRY * self.present_failures.min(8);
            self.repaint_at = Some(Instant::now() + backoff);
            return;
        }
        if self.present_failures > 0 {
            log::info!(
                "surface recovered after {} failed frame(s)",
                self.present_failures
            );
            self.present_failures = 0;
        }
        if !self.logged_first_frame {
            self.logged_first_frame = true;
            log::info!(
                "window mapped {}x{}",
                gfx.surface_config.width,
                gfx.surface_config.height
            );
        }
        if !self.logged_first_listing {
            let rows = self.tabs.active().cwd.dir.len();
            if rows > 0 {
                self.logged_first_listing = true;
                log::info!("first listing {rows} rows");
            }
        }

        // Repaint policy: egui says when it next needs a frame. Zero means
        // "immediately" (something is mid-animation); anything past the horizon
        // is the "never" sentinel and is dropped so the loop can actually
        // sleep.
        match repaint_delay {
            Some(delay) if delay.is_zero() => {
                if frame_log_enabled() {
                    log::info!("egui-zero-delay");
                }
                gfx.window.request_redraw();
            }
            Some(delay) if delay < REPAINT_HORIZON => {
                self.repaint_at = Some(Instant::now() + delay);
            }
            _ => {}
        }
    }

    /// Write the cwd-file if this quit calls for one, and say goodbye.
    fn finish(&mut self, event_loop: &ActiveEventLoop) {
        if let (Some(Quit::WriteCwd), Some(path)) = (self.quit, self.cwd_file.as_deref()) {
            // Never a URL: the file is `cd`'d into by a shell function, and
            // quitting out of a remote service or the trash has to leave the
            // shell in the local directory that session came from.
            let cwd = self.tab().cwd.path().to_path_buf();
            let cwd = match self.tab().virtual_kind() {
                Some(_) => self.local_origin(),
                None => cwd,
            };
            crate::cli::write_cwd_file(path, &cwd);
        }
        // Before the window, and therefore before the `wl_surface` the data
        // device holds a proxy to: dropping it joins its thread, which is the
        // one place that promise is kept (see `DataDevice::start`'s safety
        // note).
        self.data_device = None;
        event_loop.exit();
    }
}

/// Is this a kind the transport can act on (PLAN §4.3)?
///
/// Video and audio, and nothing else: a PDF has pages, a font has glyphs and a
/// 3D model has a turntable, and none of those is a thing `k` plays. Kept as a
/// free function so the answer is one line to read and one line to test.
fn is_temporal(kind: &PreviewKind) -> bool {
    matches!(kind, PreviewKind::Video | PreviewKind::Audio)
}

/// How a finished operation reads, and for how long.
///
/// One function so the wording is decided in one place and can be read without
/// a running worker pool: an operation with an inverse gets the 8 s undo toast
/// (PLAN §5) — the "u — undo" hint is the toast's own, so the message says only
/// what happened — and everything else says what it did or what went wrong.
fn op_toast(outcome: &df_core::ops::OpOutcome) -> (String, crate::toast::ToastKind) {
    use crate::toast::ToastKind;
    let failed = outcome.errors.len();
    let message = if failed > 0 {
        format!("{} · {} failed", outcome.message, failed)
    } else {
        outcome.message.clone()
    };
    if outcome.record.is_some() {
        // Undoable even when part of it failed: what *did* land is real, and
        // the journal is holding its inverse.
        return (message, ToastKind::Undo);
    }
    if outcome.cancelled {
        return (format!("{message} — cancelled"), ToastKind::Notice);
    }
    if failed > 0 {
        let (path, error) = &outcome.errors[0];
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        return (
            if failed == 1 {
                format!("{name}: {error}")
            } else {
                format!("{name}: {error} · {} more failed", failed - 1)
            },
            ToastKind::Error,
        );
    }
    (message, ToastKind::Notice)
}

/// What the bar says while a modal surface owns the keyboard.
fn overlay_hints(
    dialog: &Option<Dialog>,
    picker: bool,
    spot: bool,
    mounts: bool,
) -> Vec<(&'static str, &'static str)> {
    if mounts {
        // The disks card's own vocabulary, including the two `[pick]` has no
        // row for — which is exactly why they are listed here: a key that is
        // not on the help sheet has to be on the hint bar or it may as well not
        // exist.
        return vec![
            ("↑↓", "choose"),
            ("Enter", "mount / open"),
            ("u", "unmount"),
            ("e", "eject"),
            ("Esc", "close"),
        ];
    }
    if spot {
        // Every key the card answers to, including the two df-core's `[spot]`
        // table has no row for — which is exactly why they are listed here: a
        // key that is not on the help sheet has to be on the hint bar or it
        // may as well not exist.
        return vec![
            ("↑↓", "row"),
            ("←→", "previous / next file"),
            ("⇧←→", "permission bit"),
            ("Space", "toggle / hash"),
            ("Tab / Esc", "close"),
        ];
    }
    match dialog {
        Some(Dialog::Confirm(_)) => vec![
            ("Enter / y", "confirm"),
            ("Esc / n", "cancel"),
            ("↑↓", "scroll"),
        ],
        Some(Dialog::Bulk(_)) => vec![
            ("Tab / ↑↓", "next field"),
            ("Enter", "rename"),
            ("Esc", "cancel"),
        ],
        Some(Dialog::Conflict(_)) => vec![
            ("↑↓", "choose"),
            ("o s r", "overwrite / skip / rename"),
            ("a", "apply to all"),
            ("Enter", "apply"),
            ("Esc", "cancel the paste"),
        ],
        None if picker => vec![("↑↓", "choose"), ("Enter", "open"), ("Esc", "close")],
        None => vec![
            ("↑↓", "select"),
            ("p", "pause"),
            ("x", "cancel"),
            ("Enter", "inspect"),
            ("w / Esc", "close"),
        ],
    }
}

/// "1 item" / "3 items". The same wording df-core's jobs use, so a toast about
/// a paste and a toast about a link count the same way.
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Whether a path may be handed to the local directory scanner.
///
/// The panes address two places by URL — `sftp://…` and `trash://` — and both
/// are *display* paths: relative `PathBuf`s that `read_dir` would resolve
/// against the process's own directory and fail on. A rescan of one wipes the
/// listing it was supposed to refresh, which is how a remote pane could be
/// emptied by an unrelated local operation finishing.
fn scannable(dir: &Path) -> bool {
    crate::remote::at_of(dir).is_none() && dir != Path::new(crate::trashview::URL)
}

/// The directory to start a child process in, given where the pane is and the
/// real directory that pane's session came from.
///
/// Pure, so the rule is a test: anything that is not a directory on this
/// machine — a URL, or an archive's interior, which *looks* like a path and is
/// not one — falls back to the origin. The existence check is deliberate rather
/// than a list of schemes: it is the actual question `Command::current_dir` is
/// about to ask the kernel, and it catches the fourth case nobody has thought
/// of yet.
fn spawnable_cwd(pane: &Path, origin: &Path) -> PathBuf {
    if scannable(pane) && pane.is_dir() {
        return pane.to_path_buf();
    }
    origin.to_path_buf()
}

/// The command a context-menu row *is*, when it is one.
///
/// Pure, and the reason the menu cannot drift from the keyboard: every gate a
/// key passes ([`App::refuse_where_we_are`]) is expressed over
/// [`Command`], so a menu row that names the same verb is checked by the same
/// list rather than by a branch somebody remembered to write. The three rows
/// that are not commands — the opener submenu's own parent, and the two trash
/// verbs the keymap spells with `Enter`/`D` in a view that already gates them —
/// answer `None`.
fn menu_command(action: menu::Action) -> Option<Command> {
    use menu::Action as A;
    use Command as C;
    Some(match action {
        A::Open => C::Open,
        // Both the submenu and its rows launch a child process with the row's
        // path as an argument, which is exactly what `O` does.
        A::OpenWith(_) | A::OpenWithMenu => C::OpenInteractive,
        A::Yank => C::Yank,
        A::Cut => C::YankCut,
        A::Paste => C::Paste,
        A::Rename => C::Rename,
        A::Trash => C::Trash,
        A::ExtractHere => C::ArchiveExtractHere,
        A::ExtractSubfolder => C::ArchiveExtractSubfolder,
        A::CopyPath => C::CopyPath,
        A::CopyName => C::CopyFilename,
        A::Properties => C::Spot,
        A::Purge => C::DeletePermanently,
        A::Restore | A::EmptyTrash => return None,
    })
}

/// Which commands are inert while the list pane is showing the trash.
///
/// The rule is "everything whose subject would have to be the file where it
/// currently is". A trashed file *is* somewhere — inside `…/Trash/files/` —
/// and every one of these would act on it there: renaming it would break the
/// info record that says how to put it back, trashing it again is a
/// contradiction, and pasting into `trash://` is a directory that does not
/// exist. `Enter`/`r` restore, `D` destroys, and everything about *looking* —
/// the sorts, the filter, the selection, `Tab`, `Ctrl+p` — is untouched.
fn inert_in_trash(command: Command) -> bool {
    use Command as C;
    matches!(
        command,
        C::Yank
            | C::YankCut
            | C::Paste
            | C::PasteForce
            | C::SymlinkAbsolute
            | C::SymlinkRelative
            | C::Hardlink
            // Already trashed. A `d` here would be the user asking for
            // something that has happened.
            | C::Trash
            | C::Create
            | C::RenameEmptyStem
            | C::Shell
            | C::ShellBlock
            | C::SearchName
            | C::SearchContent
            | C::DiskUsage
            | C::BasketToggle
            | C::ArchiveExtractHere
            | C::ArchiveExtractSubfolder
            // Nested trash is not a place.
            | C::OpenTrash
    )
}

/// Route one remote listing update to the tab that asked for it (PLAN §7.6).
///
/// Returns whether anybody wanted it. The token is the whole staleness
/// contract, exactly as it is for a local scan: arrowing quickly through remote
/// directories leaves several listings in the air, and every one that is not
/// this tab's current token is dropped on arrival rather than landing in the
/// wrong pane.
fn apply_vfs(tab: &mut Tab, update: &df_core::vfs::VfsUpdate) -> bool {
    use df_core::vfs::VfsUpdate as U;
    let Some(session) = &mut tab.remote else {
        return false;
    };
    if session.pending.as_ref().map(|(token, _)| *token) != Some(update.token()) {
        return false;
    }
    match update {
        U::Started { .. } => tab.cwd.dir.begin_external(),
        U::Batch { entries, .. } => tab.cwd.dir.extend_external(entries.clone()),
        U::Done { dir, .. } => {
            tab.cwd.dir.finish_external();
            // Cached now that it is complete, never mid-stream: half a
            // directory in the cache would make `←` back into it show half a
            // directory and never find out.
            let rows = tab.cwd.dir.entries().to_vec();
            if let Some(session) = &mut tab.remote {
                session.store(dir, rows);
                session.pending = None;
            }
        }
        U::Failed { error, .. } => {
            tab.cwd.dir.fail_external(error.to_string());
            if let Some(session) = &mut tab.remote {
                session.pending = None;
            }
        }
    }
    true
}

/// A downloaded preview file as text, or `None` when it is not text after all.
///
/// The size gate happened before the download ([`crate::remote::previewable`]);
/// this is the second half of the same honesty, because a `.txt` full of bytes
/// is a screen of replacement characters and the facts card is the better
/// answer.
fn read_preview_text(local: &Path) -> Option<String> {
    let bytes = std::fs::read(local).ok()?;
    String::from_utf8(bytes).ok()
}

/// Gate for the per-frame `DF_FRAME_LOG` diagnostic, read once.
fn frame_log_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DF_FRAME_LOG").is_some_and(|v| v == "1"))
}

/// Which way a prompt searches.
fn find_direction(kind: PromptKind) -> FindDirection {
    match kind {
        PromptKind::FindPrev => FindDirection::Backward,
        _ => FindDirection::Forward,
    }
}

/// `N` is `n` the other way round.
fn flip(direction: FindDirection) -> FindDirection {
    match direction {
        FindDirection::Forward => FindDirection::Backward,
        FindDirection::Backward => FindDirection::Forward,
    }
}

fn sort_options(mgr: &MgrConfig, seed: u64) -> SortOptions {
    SortOptions {
        seed,
        ..SortOptions::from_config(mgr)
    }
}

/// Where to start, and what to put the cursor on.
///
/// Being handed a *file* opens its directory with the cursor on it — the shape
/// "open this in the file manager" always means, and the one yazi has.
fn start_directory(requested: Option<&Path>) -> (PathBuf, Option<String>) {
    let fallback = || std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let Some(path) = requested else {
        return (fallback(), None);
    };
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => (path.to_path_buf(), None),
        Ok(_) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            (
                path.parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(fallback),
                name,
            )
        }
        Err(e) => {
            log::warn!("{}: {e}; opening the current directory", path.display());
            (fallback(), None)
        }
    }
}

/// Put the start-up cursor on `name` if it is there yet, and say whether it is
/// still worth trying again.
///
/// The whole point is the second half. A directory read is asynchronous, so the
/// name a person typed on the command line arrives *before* the row it belongs
/// on; a placement attempted once, at startup, always fails, and the cursor
/// sits on row 0 as though the argument had been ignored. Retrying on every
/// batch fixes it, and the retries have to stop somewhere — which is when the
/// scan finishes without the file, because it was deleted between the shell and
/// here, or is hidden and `.` is off.
///
/// Pure, so both halves of that are a test rather than something you find out
/// by opening a file from a shell.
fn place_start_cursor(dir: &mut df_core::fs::DirState, name: &str) -> bool {
    if dir.cursor_to_name(name) {
        return false;
    }
    matches!(
        dir.state(),
        df_core::fs::LoadState::Idle | df_core::fs::LoadState::Loading
    )
}

/// Collapse a chain of renames into where each file started and ended.
///
/// A swap runs as three moves — `a`→`tmp`, `b`→`a`, `tmp`→`b` — and the
/// temporary is an implementation detail of doing it one syscall at a time. The
/// journal must not see it: a record saying `tmp`→`b` would, on `u`, put a file
/// back to a name that never existed as far as the user is concerned, and the
/// verify would fail besides.
///
/// So consecutive legs are stitched: whenever one rename's destination is a
/// later rename's source, the two become one, keeping the *first* origin and the
/// *last* destination — and the fingerprint of the last, which is the file as it
/// finally sits on disk.
fn collapse_renames(moved: Vec<MovedPath>) -> Vec<MovedPath> {
    let mut out: Vec<MovedPath> = Vec::new();
    for leg in moved {
        match out.iter_mut().find(|prior| prior.to == leg.from) {
            Some(prior) => {
                prior.to = leg.to;
                prior.fingerprint = leg.fingerprint;
            }
            None => out.push(leg),
        }
    }
    // A file that ended up back where it started is not a rename at all.
    out.retain(|leg| leg.from != leg.to);
    out
}

/// The spot panel's git row: the branch, and what git says about this path.
///
/// `None` when the file is not in a repository, or when the first status scan
/// has been queued and has not landed — an absent row rather than a row saying
/// "loading", because a fact that is not known yet is not a fact.
fn git_line(git: &df_core::git::Git, path: &Path) -> Option<String> {
    use df_core::git::FileStatus;
    let status = git.ensure(path)?;
    let branch = status.branch().unwrap_or("HEAD").to_string();
    let word = match status.status_for(path) {
        Some(FileStatus::Ignored) => "ignored",
        Some(FileStatus::Untracked) => "untracked",
        Some(FileStatus::Added) => "added",
        Some(FileStatus::Deleted) => "deleted",
        Some(FileStatus::Renamed) => "renamed",
        Some(FileStatus::Typechange) => "type changed",
        Some(FileStatus::Modified) => "modified",
        Some(FileStatus::Conflict) => "conflicted",
        // Tracked and clean: git has an opinion about the repository and none
        // about this file, which is the good news.
        None => "unchanged",
    };
    Some(format!("{branch} · {word}"))
}

/// The nearest ancestor of `path` that still exists — where to go when the
/// directory you were in was deleted underneath you. `/` always qualifies.
fn nearest_existing(path: &Path) -> PathBuf {
    let mut candidate = path;
    while let Some(parent) = candidate.parent() {
        if parent.is_dir() {
            return parent.to_path_buf();
        }
        candidate = parent;
    }
    PathBuf::from("/")
}

impl ApplicationHandler<crate::Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_none() {
            if let Err(e) = self.init_gfx(event_loop) {
                log::error!("{e}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(gfx) = &mut self.gfx else { return };
        // One process, one window (PLAN §2 — see [`crate::window`] for why a
        // second window is a second process). The id is still checked rather
        // than ignored: it is the invariant written down where it holds, and if
        // an in-process second window is ever added this is the line that turns
        // into the routing table instead of a silent misdelivery.
        if gfx.window.id() != id {
            return;
        }
        let response = gfx.egui_state.on_window_event(&gfx.window, &event);
        // egui-winit answers `repaint: true` to `RedrawRequested` itself, so
        // honouring it unconditionally is a vsync-paced self-loop — the app
        // repaints forever at 60 fps while idle (measured with DF_FRAME_LOG).
        // After a redraw, the *next* frame is decided by `redraw()`'s own
        // policy (animating / deadline / egui's repaint_delay), never by the
        // event that delivered this one.
        if response.repaint && !matches!(event, WindowEvent::RedrawRequested) {
            if frame_log_enabled() {
                log::info!("event-repaint: {event:?}");
            }
            self.watchdog.redraw_requested();
            gfx.window.request_redraw();
        }
        match event {
            // Closing the window is a `q`, not a `Q`: the wrapper script should
            // follow you to wherever you finished.
            WindowEvent::CloseRequested => {
                self.quit = Some(Quit::WriteCwd);
                self.finish(event_loop);
            }
            WindowEvent::Resized(size) => {
                gfx.resize(size.width, size.height);
                gfx.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                // Key repeat is kept: holding `↓` has to scroll, and the keymap
                // treats a repeat exactly as a press.
                let chord = crate::keys::chord(&event, self.modifiers);
                let text = crate::keys::text(&event);
                if chord.is_some() || text.is_some() {
                    self.pending_keys.push(Press {
                        repeat: event.repeat,
                        chord,
                        text,
                    });
                    gfx.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                self.redraw();
                if self.quit.is_some() {
                    self.finish(event_loop);
                }
            }
            _ => {}
        }
    }

    /// A worker has something, or egui asked for a frame off-thread. The only
    /// cross-thread path into the loop.
    ///
    /// The redraw is unconditional rather than gated on `poll_workers`: a
    /// `Wake` is only ever sent by something that has already decided a frame
    /// is warranted, so second-guessing it here would drop egui's own requests
    /// on the floor. It stays event-driven — no `Wake`, no frame.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: crate::Wake) {
        if frame_log_enabled() {
            log::info!("wake");
        }
        self.poll_workers();
        if let Some(gfx) = &self.gfx {
            self.watchdog.redraw_requested();
            gfx.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        match self.repaint_at {
            Some(at) if at <= now => {
                self.repaint_at = None;
                if frame_log_enabled() {
                    log::info!("deadline-fire");
                }
                if let Some(gfx) = &self.gfx {
                    gfx.window.request_redraw();
                }
            }
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            // The resting state, and the one that has to stay reachable: no
            // deadline, no poll, no frame until something happens.
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    /// winit's last callback, and the only point at which the Wayland
    /// connection is still alive. egui-winit's clipboard worker must be joined
    /// here rather than when `run_app` drops us — the same SIGSEGV-at-quit
    /// delightviewer hit — so anything holding a platform resource is dropped
    /// in this window and not later.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // PLAN §2's other half of the debounce: whatever the timer has not
        // written yet is written now, because there is no later. Written first,
        // before anything that can take time, so a slow worker shutdown cannot
        // eat the state file.
        self.flush_state();
        // PLAN §7.6: every file a remote session downloaded goes with it. The
        // ledger names them all — the previews, the download-on-opens, the
        // stale copies of renamed rows — and this is the one sweep.
        if !self.temps.is_empty() {
            let held = self.temps.len();
            let swept = self.temps.clear();
            log::info!("removed {swept} of {held} temporary remote file(s)");
        }
        // Stop the workers before the window goes: a scan that finished into a
        // dropped channel is harmless, but joining them here keeps the shutdown
        // order the same every time.
        self.scanner.cancel_all();
        // The task engine joins its workers when it is dropped, so anything
        // still running has to be told to stop *first* — otherwise closing the
        // window during a 40 GB copy leaves a dead window on screen until the
        // copy finishes. Cancelling is safe: a cancelled copy removes its own
        // partial destination (`ops::copy`), and what already landed is real
        // and journalled.
        let running = self.engine.active_count();
        if running > 0 {
            log::warn!("quitting with {running} task(s) still running; cancelling them");
        }
        self.engine.cancel_all();
        // The audio device and the decode thread go before the window does: a
        // controller still holding a cpal stream while the Wayland connection
        // is torn down is the shape of crash-at-quit this program's ordering
        // exists to avoid.
        if let Some(player) = &mut self.player {
            player.shutdown();
        }
        self.player = None;
        self.gfx = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The temporary leg of a swap is an implementation detail of running the
    /// renames one at a time, and the journal must not see it: `u` has to put
    /// the file back where it *started*, not to a name that was only ever a
    /// detour.
    #[test]
    fn a_swaps_temporary_leg_never_reaches_the_journal() {
        use df_core::ops::journal::{FileKind, Fingerprint};
        let print = Fingerprint {
            kind: FileKind::File,
            len: 0,
            mtime: None,
            entries: None,
        };
        let leg = |from: &str, to: &str| MovedPath {
            from: PathBuf::from(from),
            to: PathBuf::from(to),
            fingerprint: print.clone(),
        };
        // `a`→`tmp`, `b`→`a`, `tmp`→`b`: the three moves a swap actually runs.
        let collapsed = collapse_renames(vec![
            leg("/d/a", "/d/b.df-rename-1"),
            leg("/d/b", "/d/a"),
            leg("/d/b.df-rename-1", "/d/b"),
        ]);
        assert_eq!(collapsed.len(), 2, "{collapsed:?}");
        let pairs: Vec<(PathBuf, PathBuf)> = collapsed
            .iter()
            .map(|m| (m.from.clone(), m.to.clone()))
            .collect();
        assert!(pairs.contains(&(PathBuf::from("/d/a"), PathBuf::from("/d/b"))));
        assert!(pairs.contains(&(PathBuf::from("/d/b"), PathBuf::from("/d/a"))));
        // Nothing in the record mentions the detour.
        assert!(collapsed
            .iter()
            .all(|m| !m.to.to_string_lossy().contains("df-rename")));

        // A plain batch passes through untouched…
        let plain = collapse_renames(vec![leg("/d/x", "/d/y"), leg("/d/p", "/d/q")]);
        assert_eq!(plain.len(), 2);
        // …and a file that ends up back where it started is not a rename.
        let round_trip = collapse_renames(vec![leg("/d/x", "/d/t"), leg("/d/t", "/d/x")]);
        assert!(round_trip.is_empty());
    }

    /// PLAN §2's flush contract, both halves of it: df-core's store decides
    /// there is something to save, this side decides when — and a change while
    /// one is already pending pushes the deadline out rather than queueing a
    /// second write.
    #[test]
    fn the_state_file_is_written_behind_a_debounce_and_not_before() {
        let t0 = Instant::now();
        let mut timer = WriteBehind::default();
        // Nothing to save is nothing to schedule: the resting state costs no
        // wake-up at all.
        timer.touch(false, t0);
        assert_eq!(timer.deadline(t0), None);
        assert!(!timer.ready(t0));

        timer.touch(true, t0);
        assert_eq!(timer.deadline(t0), Some(STATE_FLUSH));
        assert!(!timer.ready(t0), "not yet");
        assert!(!timer.ready(t0 + STATE_FLUSH / 2));
        // A second change while one is pending is a *debounce*, not a second
        // write: the deadline moves.
        timer.touch(true, t0 + STATE_FLUSH / 2);
        assert!(!timer.ready(t0 + STATE_FLUSH));
        assert!(timer.ready(t0 + STATE_FLUSH + STATE_FLUSH));
        // One arming is one write, and afterwards nothing is owed a frame.
        assert!(!timer.ready(t0 + STATE_FLUSH * 4));
        assert_eq!(timer.deadline(t0 + STATE_FLUSH * 4), None);

        // …and the quit path, which has no later.
        timer.touch(true, t0);
        timer.disarm();
        assert_eq!(timer.deadline(t0), None);
        assert!(!timer.ready(t0 + STATE_FLUSH * 4));
    }

    /// And the store it drives really does round-trip a directory's view, so
    /// the debounce is protecting something that works (PLAN §2's per-directory
    /// memory).
    #[test]
    fn a_grid_toggle_survives_a_write_and_a_reload() {
        // A throwaway path under `$TMPDIR`. df-core's own `TempTree` is
        // `#[cfg(test)]` and does not cross the crate boundary; one file needs
        // one path, so this is it.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("df-app-state-{nanos}"));
        let _cleanup = scopeguard(&path);
        let mut store = StateStore::load_from(&path);
        assert_eq!(store.view(Path::new("/pictures")), None, "list by default");
        store.set_view("/pictures", Some(View::Grid));
        assert!(store.is_dirty());
        store.flush().expect("the state file writes");
        assert!(!store.is_dirty(), "flushing clears the dirt");

        let reloaded = StateStore::load_from(&path);
        assert_eq!(reloaded.view(Path::new("/pictures")), Some(View::Grid));
        assert_eq!(reloaded.view(Path::new("/elsewhere")), None);

        // Back to the default is stored as *no preference* rather than as a
        // record, so a folder glanced at in a grid and switched back does not
        // live in the file for ever.
        let mut store = StateStore::load_from(&path);
        store.set_view("/pictures", None);
        store.flush().expect("the state file writes");
        assert!(StateStore::load_from(&path).is_empty());
    }

    /// Removes a test's file when it goes out of scope, so a failing assertion
    /// does not leave one behind for the next run to read.
    fn scopeguard(path: &Path) -> impl Drop {
        struct Remove(PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        Remove(path.to_path_buf())
    }

    #[test]
    fn a_file_argument_opens_its_directory_with_it_under_the_cursor() {
        let file = std::env::current_exe().expect("test binary path");
        let (dir, focus) = start_directory(Some(&file));
        assert_eq!(Some(dir.as_path()), file.parent());
        assert_eq!(
            focus,
            file.file_name().map(|n| n.to_string_lossy().into_owned())
        );
    }

    /// **The bug this fixes**: `Tab::open` queues the directory read, so at the
    /// moment `App::new` runs there is nothing in the listing to point at and a
    /// single `cursor_to_name` finds nothing. The name has to be remembered and
    /// retried as the batches land.
    ///
    /// Driven against a real scanner and a real directory, because the failure
    /// is entirely about *ordering* — a mocked listing that already had the rows
    /// in it would pass with the bug still in place.
    #[test]
    fn the_start_up_cursor_waits_for_the_scan_that_will_contain_it() {
        use df_core::fs::{no_notifier, LoadState};

        // A real directory, made by hand rather than through df-core's
        // `test-support` fixture: this is the binary crate, and pulling a
        // feature of another crate in for one `mkdir` is not worth it.
        let tree = std::env::temp_dir().join(format!("df-start-cursor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tree);
        std::fs::create_dir_all(&tree).expect("make the fixture directory");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(tree.join(name), b"x").expect("write the fixture");
        }
        let scanner = Scanner::start(no_notifier());
        let now = Instant::now();
        let mgr = MgrConfig::default();
        let sort = sort_options(&mgr, 0);
        let mut tab = Tab::open(tree.clone(), &mgr, sort, &scanner, now);

        // Before anything has arrived: the placement fails, and it says so by
        // asking to be tried again. This is exactly the state `App::new` is in.
        assert!(
            place_start_cursor(&mut tab.cwd.dir, "c.txt"),
            "an empty listing must ask to be retried, not give up"
        );
        assert_eq!(tab.cwd.dir.cursor(), 0);

        // Now let the scan land, retrying on each update the way `poll_workers`
        // does.
        let mut waiting = true;
        let deadline = Instant::now() + Duration::from_secs(5);
        while waiting && Instant::now() < deadline {
            for update in scanner.drain() {
                tab.apply(&update);
                if waiting {
                    waiting = place_start_cursor(&mut tab.cwd.dir, "c.txt");
                }
            }
            if waiting {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        assert!(!waiting, "the scan never landed");
        assert_eq!(tab.cwd.dir.state(), LoadState::Loaded);
        let name = tab.cwd.dir.cursor_entry().map(|e| e.name.clone());
        assert_eq!(name.as_deref(), Some("c.txt"), "the cursor did not follow");

        // …and a name that is not in the directory gives up once the scan is
        // over rather than retrying for the life of the process.
        assert!(
            !place_start_cursor(&mut tab.cwd.dir, "nope.txt"),
            "a finished scan without the file must stop the retries"
        );

        let _ = std::fs::remove_dir_all(&tree);
    }

    #[test]
    fn a_directory_argument_opens_it() {
        let (dir, focus) = start_directory(Some(Path::new("/")));
        assert_eq!(dir, PathBuf::from("/"));
        assert_eq!(focus, None);
    }

    #[test]
    fn a_path_that_is_not_there_falls_back_to_the_current_directory() {
        let (dir, focus) = start_directory(Some(Path::new("/nonexistent/delightfile-test")));
        assert_eq!(Some(dir), std::env::current_dir().ok());
        assert_eq!(focus, None);
    }

    /// PLAN §5: an operation with an inverse lands with the undo toast; the
    /// "u — undo" hint is the toast's, so the message is only what happened.
    #[test]
    fn an_undoable_operation_gets_the_undo_toast() {
        use crate::toast::ToastKind;
        use df_core::ops::OpOutcome;

        let copied = OpOutcome {
            record: Some(OpRecord::Copy {
                created: Vec::new(),
            }),
            message: "Copied 3 items".to_string(),
            ..Default::default()
        };
        assert_eq!(
            op_toast(&copied),
            ("Copied 3 items".to_string(), ToastKind::Undo)
        );
        assert_eq!(ToastKind::Undo.lifetime(), crate::toast::UNDO_LIFETIME);

        // A partial success is still undoable, and still says how much failed.
        let partial = OpOutcome {
            record: Some(OpRecord::Trash { items: Vec::new() }),
            message: "Trashed 2 items".to_string(),
            errors: vec![(PathBuf::from("/srv/x"), "permission denied".to_string())],
            ..Default::default()
        };
        assert_eq!(
            op_toast(&partial),
            ("Trashed 2 items · 1 failed".to_string(), ToastKind::Undo)
        );

        // A permanent delete has no inverse, so it gets a plain notice.
        let deleted = OpOutcome {
            message: "Deleted 1 item".to_string(),
            ..Default::default()
        };
        assert_eq!(
            op_toast(&deleted),
            ("Deleted 1 item".to_string(), ToastKind::Notice)
        );

        // A total failure names the file and says what went wrong, in df-core's
        // own words.
        let failed = OpOutcome {
            message: "Deleted 0 items".to_string(),
            errors: vec![
                (PathBuf::from("/"), "refusing to delete /".to_string()),
                (PathBuf::from("/etc"), "permission denied".to_string()),
            ],
            ..Default::default()
        };
        let (message, kind) = op_toast(&failed);
        assert_eq!(kind, ToastKind::Error);
        assert!(message.starts_with("/: refusing to delete /"), "{message}");
        assert!(message.ends_with("· 1 more failed"), "{message}");

        let cancelled = OpOutcome {
            message: "Copied 1 item".to_string(),
            cancelled: true,
            ..Default::default()
        };
        assert_eq!(
            op_toast(&cancelled),
            ("Copied 1 item — cancelled".to_string(), ToastKind::Notice)
        );
    }

    /// The spot panel's keyboard, through the **real** registry: `Tab` opens
    /// it from the browser and closes it from inside, `←`/`→` swipe, `↑`/`↓`
    /// walk the rows, and `Esc` and `Ctrl+c` both put it away.
    ///
    /// PLAN §4.1's note on the overlays is the thing this pins: yazi swipes the
    /// spot with `h`/`l`, and §4.3 hard-reserves those, so the arrows carry it
    /// here and nothing else may claim them while the card is up.
    #[test]
    fn the_spot_panel_answers_the_keys_the_card_advertises() {
        let registry = Registry::defaults();
        let dispatch = |context: Context, key: &str| {
            let mut state = KeymapState::new();
            let chord = df_core::keymap::parse_chord(key).expect("chord");
            let stack = ContextStack::with(&[context]);
            match registry.dispatch(&mut state, &stack, WhenFlags::LIST, chord, Instant::now()) {
                Dispatch::Match(command) => Some(command),
                _ => None,
            }
        };
        assert_eq!(dispatch(Context::Files, "tab"), Some(Command::Spot));
        for key in ["tab", "esc", "ctrl+c"] {
            assert_eq!(
                dispatch(Context::Spot, key),
                Some(Command::OverlayClose),
                "`{key}` did not close the card"
            );
        }
        assert_eq!(
            dispatch(Context::Spot, "left"),
            Some(Command::SpotSwipePrev)
        );
        assert_eq!(
            dispatch(Context::Spot, "right"),
            Some(Command::SpotSwipeNext)
        );
        assert_eq!(dispatch(Context::Spot, "up"), Some(Command::OverlayPrev));
        assert_eq!(dispatch(Context::Spot, "down"), Some(Command::OverlayNext));
        // …and the two the table has no row for are handled literally, so they
        // must *not* resolve to something else by accident.
        assert_eq!(dispatch(Context::Spot, "space"), None);
        assert_eq!(dispatch(Context::Spot, "shift+left"), None);
    }

    /// Every modal surface says what its keys do, and never claims a key the
    /// surface does not have.
    #[test]
    fn each_overlay_teaches_its_own_keys() {
        let confirm = overlay_hints(
            &Some(Dialog::Confirm(Confirm::new(
                ConfirmKind::Delete,
                vec![PathBuf::from("/tmp/a")],
            ))),
            false,
            false,
            false,
        );
        assert!(confirm.iter().any(|(k, _)| k.contains("Enter")));
        assert!(confirm.iter().any(|(k, _)| k.contains("Esc")));
        let panel = overlay_hints(&None, false, false, false);
        assert!(panel.iter().any(|(k, what)| *k == "x" && *what == "cancel"));
        let picker = overlay_hints(&None, true, false, false);
        assert!(picker.iter().any(|(_, what)| *what == "open"));
        // The spot's hints cover the two keys df-core's `[spot]` table has no
        // row for, which is the only place they are ever advertised.
        let spot = overlay_hints(&None, false, true, false);
        assert!(spot.iter().any(|(k, _)| k.contains("Space")));
        assert!(spot.iter().any(|(k, _)| k.contains('⇧')));
        assert!(spot.iter().any(|(k, _)| k.contains("Tab")));
        // The disks card advertises the two verbs `[pick]` has no row for.
        let mounts = overlay_hints(&None, false, false, true);
        assert!(mounts.iter().any(|(k, what)| *k == "e" && *what == "eject"));
        assert!(mounts
            .iter()
            .any(|(k, what)| *k == "u" && *what == "unmount"));
    }

    /// **Only video and audio grow a transport** (PLAN §4.3). A PDF has pages
    /// and a font has glyphs; `k` on either must do nothing at all, which is
    /// what keeps the reserved keys from being a surprise on the wrong file.
    #[test]
    fn the_transport_keys_are_live_on_clips_and_nothing_else() {
        assert!(is_temporal(&PreviewKind::Video));
        assert!(is_temporal(&PreviewKind::Audio));
        for kind in [
            PreviewKind::Image,
            PreviewKind::Pdf,
            PreviewKind::Font,
            PreviewKind::Model3d,
            PreviewKind::Gcode,
            PreviewKind::Archive,
            PreviewKind::Directory,
            PreviewKind::Binary,
            PreviewKind::Markdown,
            PreviewKind::Text { syntax: None },
        ] {
            assert!(!is_temporal(&kind), "{kind:?}");
        }
        // …and the classification a hovered row gets is the name-based one, so
        // `l` is live the instant the cursor lands rather than a preview round
        // trip later.
        let mime = df_core::fs::mime::hint_for_name("clip.mkv");
        assert_eq!(
            df_core::preview::kind_for_mime(mime, "clip.mkv"),
            PreviewKind::Video
        );
        let mime = df_core::fs::mime::hint_for_name("voice.opus");
        assert_eq!(
            df_core::preview::kind_for_mime(mime, "voice.opus"),
            PreviewKind::Audio
        );
    }

    /// **The bug this fixes**: `launch` and `run_shell` handed the *pane's*
    /// path to `Command::current_dir`. On a remote service that is
    /// `sftp://host/srv` and on the trash it is `trash://` — neither is a
    /// directory, so `o` on a remote file downloaded it perfectly and then
    /// failed to open it, and an opener with a relative argument would have
    /// resolved it somewhere else entirely. It worked by luck everywhere else:
    /// the pane usually *is* a real directory.
    #[test]
    fn a_child_process_is_never_started_in_a_place_that_is_not_a_directory() {
        let origin = std::env::temp_dir();
        let real = std::env::current_dir().expect("a working directory");

        // A local pane is its own working directory — nothing changes for the
        // overwhelmingly common case.
        assert_eq!(spawnable_cwd(&real, &origin), real);

        // The three that are not directories on this machine.
        assert_eq!(
            spawnable_cwd(Path::new("sftp://showandtour1/srv/www"), &origin),
            origin
        );
        assert_eq!(
            spawnable_cwd(Path::new(crate::trashview::URL), &origin),
            origin
        );
        // An archive's interior *looks* like a path, which is exactly why the
        // question asked is "is this a directory" and not "does it start with a
        // scheme".
        let inside = real.join("archive.zip/inner");
        assert_eq!(spawnable_cwd(&inside, &origin), origin);

        // …and the same rule keeps the scanner off them.
        assert!(scannable(&real));
        assert!(!scannable(Path::new("sftp://showandtour1/srv")));
        assert!(!scannable(Path::new(crate::trashview::URL)));
    }

    /// The context menu is a second dispatch over the same verbs, and it must
    /// be gated by the same list.
    ///
    /// **The bug this fixes**: `menu_action` walked past `inert_remotely`
    /// entirely — "Open with…" on a remote row launched a viewer on the string
    /// `sftp://host/photo.png`, and `d` was safe only because somebody had
    /// hand-written a remote branch for that one row.
    #[test]
    fn every_menu_row_that_is_a_verb_is_gated_by_that_verbs_own_rules() {
        use df_core::keymap::Command as C;
        use menu::Action as A;
        assert_eq!(menu_command(A::OpenWith(2)), Some(C::OpenInteractive));
        assert_eq!(menu_command(A::OpenWithMenu), Some(C::OpenInteractive));
        assert_eq!(menu_command(A::Cut), Some(C::YankCut));
        assert_eq!(menu_command(A::Rename), Some(C::Rename));
        assert_eq!(menu_command(A::Trash), Some(C::Trash));
        // The two rows that are not verbs the keymap has: the trash view gates
        // them itself, and mapping them to something they are not would be the
        // very drift this function exists to stop.
        assert_eq!(menu_command(A::Restore), None);
        assert_eq!(menu_command(A::EmptyTrash), None);

        // The rows that would act locally on a remote row are refused by the
        // one list, without a branch of their own.
        for action in [A::OpenWith(0), A::OpenWithMenu, A::Cut] {
            let command = menu_command(action).expect("a verb");
            assert!(
                crate::remote::inert_remotely(command),
                "{action:?} must not run over the link"
            );
        }
        // …and the ones that genuinely work remotely still do.
        for action in [A::Open, A::Yank, A::Paste, A::Rename, A::Trash] {
            let command = menu_command(action).expect("a verb");
            assert!(
                !crate::remote::inert_remotely(command),
                "{action:?} still works over the link"
            );
        }
        // Extraction from the menu is still live inside an archive, which is
        // the one place it is most wanted.
        for action in [A::ExtractHere, A::ExtractSubfolder] {
            let command = menu_command(action).expect("a verb");
            assert!(!crate::archive::inert_in_archive(command));
        }
    }

    #[test]
    fn counting_reads_like_a_person_wrote_it() {
        assert_eq!(plural(1, "item", "items"), "1 item");
        assert_eq!(plural(0, "item", "items"), "0 items");
        assert_eq!(plural(3, "item", "items"), "3 items");
    }

    /// Deleting the directory you are standing in must land somewhere real.
    #[test]
    fn the_nearest_existing_ancestor_is_found() {
        assert_eq!(
            nearest_existing(Path::new("/nonexistent/a/b/c")),
            PathBuf::from("/")
        );
        let exe = std::env::current_exe().expect("test binary path");
        let parent = exe.parent().expect("a parent").to_path_buf();
        assert_eq!(nearest_existing(&exe.join("gone")), parent);
    }
}
