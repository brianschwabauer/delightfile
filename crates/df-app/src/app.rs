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

use std::collections::HashSet;
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
use df_core::tasks::{FnJob, Lane, TaskCtx, TaskEngine, TaskEvent, TaskId, TaskState};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::chrome;
use crate::dialog::{self, Confirm, ConfirmKind, ConflictDialog, Step};
use crate::graphics::{Gfx, GfxError};
use crate::help::{self, Help};
use crate::hover::Hovers;
use crate::input::{Prompt, PromptKind};
use crate::open::{self, Picker};
use crate::panel::{self, TaskPanel, TaskRow};
use crate::preview::Pane as PreviewPane;
use crate::ripple::Ripples;
use crate::toast::Toasts;
use crate::select::{self, Visual};
use crate::tab::Tab;
use crate::tabs::Tabs;
use crate::theme::Palette;
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

/// How long a directory read may take before the pane admits it is reading.
///
/// The first batch is 64 entries (`df_core::fs::FIRST_BATCH`) and normally
/// lands inside a millisecond, so a "loading" label would be a flash nobody can
/// read and everybody notices. 150 ms is the usual threshold for "this is
/// taking a moment" — long enough that a local directory never trips it, short
/// enough that a sleeping disk or a dead NFS mount does not leave the pane
/// looking empty and wrong.
const LOADING_DELAY: Duration = Duration::from_millis(150);

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
pub struct Waker(Arc<dyn Fn() + Send + Sync>);

impl Waker {
    pub fn new(proxy: EventLoopProxy<crate::Wake>) -> Waker {
        Waker(Arc::new(move || {
            let _ = proxy.send_event(crate::Wake);
        }))
    }

    /// Ask the event loop for a pass through `user_event`.
    pub fn wake(&self) {
        (self.0)()
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

/// The modal card that is up, if one is.
enum Dialog {
    /// `d` / `D`.
    Confirm(Confirm),
    /// A paste that hit a name that is taken. Boxed: it carries a whole
    /// [`PastePlan`](df_core::ops::paste::PastePlan) and the enum is otherwise
    /// a few words wide.
    Conflict(Box<ConflictDialog>),
}

/// One frame's worth of "where the open surface's pieces are".
enum OverlayGeom {
    Confirm(dialog::Geometry),
    Conflict(dialog::Geometry),
    Picker(egui::Rect, Vec<egui::Rect>),
    Panel(egui::Rect, Vec<egui::Rect>, Vec<TaskRow>),
}

impl OverlayGeom {
    /// What the pointer is over. `None` inside the card but not on anything is
    /// still "inside the card" as far as the caller is concerned — the modal
    /// swallows the pointer either way (see the hit test in `frame`).
    fn hit(&self, pos: egui::Pos2) -> Option<Control> {
        match self {
            OverlayGeom::Confirm(g) | OverlayGeom::Conflict(g) => g
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
            _ => None,
        }
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
    logged_first_frame: bool,

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
    tabs: Tabs,
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
    /// The last `/` or `?`, so `n` and `N` have something to repeat.
    last_find: Option<(String, FindDirection)>,

    // ── Overlays ────────────────────────────────────────────────────────────
    /// The `~` / `F1` help browser's view state, while it is open.
    help: Option<Help>,
    /// The help browser's own `f` filter. Held here rather than in the prompt
    /// so that submitting the filter can close the bar without also throwing
    /// away what was typed into it.
    help_query: String,
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
    /// Where the cursor row was last drawn: what a rename popup and the opener
    /// picker anchor themselves to (PLAN §4.2, §6).
    cursor_rect: egui::Rect,

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

        let notifier: df_core::fs::Notifier = {
            let waker = waker.clone();
            Arc::new(move || waker.wake())
        };
        let scanner = Scanner::start(Arc::clone(&notifier));
        let watcher = Watcher::start(Arc::clone(&notifier));
        // Before the window, with the scanner (PLAN §6's cold-start ordering:
        // "decode workers started **before** the window").
        let mut preview = PreviewPane::start(notifier);

        // Before the window as well (PLAN §6's cold-start ordering), and wired
        // to the same bell every other worker rings.
        let engine = TaskEngine::new(&config.tasks);
        {
            let waker = waker.clone();
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
        if let Some(name) = focus {
            // Opening on a *file* puts the cursor on it once its row arrives.
            // Set now as well, so a blocking-fast scan is not missed.
            tab.cwd.dir.cursor_to_name(&name);
        }
        watcher.watch(tab.watched());

        App {
            gfx: None,
            waker,
            repaint_at: None,
            logged_first_frame: false,
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
            tabs: Tabs::new(tab),
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
            cursor_rect: egui::Rect::ZERO,
            pending_keys: Vec::new(),
            modifiers: ModifiersState::empty(),
            prompt: None,
            visual: None,
            last_find: None,
            help: None,
            help_query: String::new(),
            which: WhichKey::new(),
            which_rows: Vec::new(),
            hovers: Hovers::new(),
            cursor_glow: Hovers::new(),
            ripples: Ripples::new(),
            nerd: false,
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
        let waker = self.waker.clone();
        gfx.egui_ctx.set_request_repaint_callback(move |info| {
            if info.delay.is_zero() {
                waker.wake();
            }
        });

        self.gfx = Some(gfx);
        Ok(())
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
        for update in self.scanner.drain() {
            // Every tab, not only the active one: a tab opened a moment ago is
            // still loading behind the strip.
            if self.tabs.apply(&update) {
                changed = true;
            }
        }
        if changed {
            // The parent's marker follows the path, and the row it belongs on
            // may only just have arrived in a batch.
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

    fn rescan(&mut self, dir: &Path, now: Instant) {
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
        let (mgr, sort) = (self.mgr.clone(), self.sort());
        self.tabs
            .active_mut()
            .rescan_all(&mgr, sort, &self.scanner, now);
        self.rewatch();
    }

    fn navigate(&mut self, path: PathBuf, now: Instant) {
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
            TaskState::Done | TaskState::Cancelled => self.finish_op(event.id, now),
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
    fn unyank(&mut self, now: Instant) {
        if self.clipboard.is_empty() {
            return;
        }
        self.clipboard.clear();
        self.toasts.notice("Clipboard cleared", now);
    }

    /// `p` / `P`. Conflicts open the dialog; a settled plan goes straight to
    /// the pool.
    fn paste(&mut self, force: bool, now: Instant) {
        if self.clipboard.is_empty() {
            self.toasts.notice("Nothing yanked — y copies, x cuts", now);
            return;
        }
        let dest = self.cwd();
        let plan = match plan_paste(&self.clipboard, &dest, force) {
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
        let paths = self.targets();
        if paths.is_empty() {
            self.toasts.notice("Nothing selected", now);
            return;
        }
        self.dialog = Some(Dialog::Confirm(Confirm::new(kind, paths)));
        self.sync_context();
    }

    /// The confirm was answered yes.
    fn run_confirm(&mut self, confirm: Confirm, _now: Instant) {
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
        let cwd = self.cwd();
        let paths = self.clipboard.paths.clone();
        let mut made = 0;
        let mut failure = None;
        for target in &paths {
            let Some(name) = target.file_name() else { continue };
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
            self.toasts
                .notice("Extraction lands with archives", now);
            return;
        }
        if choice.block {
            self.run_shell(&choice.command.clone(), paths, true, now);
            return;
        }
        let cwd = self.cwd();
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
        let cwd = self.cwd();
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

    // ── Commands ────────────────────────────────────────────────────────────

    /// Turn queued keystrokes into commands and run them.
    ///
    /// `page` is how many rows are on screen, which is what `Ctrl+f` and
    /// `Ctrl+d` are measured in — hence keys being routed mid-frame, once the
    /// panes have been laid out.
    fn route_keys(&mut self, page: usize, now: Instant) {
        // Only the list is focusable in Phase 1 (PLAN §2.1's pane focus lands
        // with the preview in Phase 3), and nothing playable can be hovered
        // until there is a media pipeline to say so.
        let flags = WhenFlags::LIST;
        for press in std::mem::take(&mut self.pending_keys) {
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
                self.overlay_key(chord, now);
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
        self.dialog.is_some() || self.picker.is_some() || self.panel.is_some()
    }

    /// The context an open surface is matched in. Never stacked on `Files`:
    /// see [`App::route_keys`].
    fn overlay_stack(&self) -> ContextStack {
        let context = if self.dialog.is_some() {
            Context::Confirm
        } else if self.picker.is_some() {
            Context::Pick
        } else {
            Context::Tasks
        };
        ContextStack::with(&[context])
    }

    fn overlay_key(&mut self, chord: Chord, now: Instant) {
        if self.overlay_literal(chord, now) {
            return;
        }
        let stack = self.overlay_stack();
        let dispatch = self
            .keymap
            .dispatch(&mut self.keys, &stack, WhenFlags::LIST, chord, now);
        let Dispatch::Match(command) = dispatch else {
            return;
        };
        use Command as C;
        match command {
            // Only the overlay vocabulary is honoured. `Global` is still under
            // the stack — that is where `Esc` lives — but a `Ctrl+p` palette or
            // a `~` help sheet opening *behind* a modal card would be a second
            // surface nobody asked for.
            C::Escape | C::OverlayClose => self.close_overlay(now),
            C::OverlaySubmit => self.submit_overlay(now),
            C::OverlayPrev => self.overlay_move(-1),
            C::OverlayNext => self.overlay_move(1),
            C::TaskInspect => {
                if let Some(panel) = &mut self.panel {
                    panel.inspect = !panel.inspect;
                }
            }
            C::TaskCancel => self.cancel_selected_task(now),
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
        if self.panel.is_some() && plain && chord.key == Key::Char('p') {
            self.pause_selected_task(now);
            return true;
        }
        false
    }

    fn overlay_move(&mut self, delta: isize) {
        match &mut self.dialog {
            Some(Dialog::Confirm(confirm)) => {
                confirm.scroll_by(delta);
                return;
            }
            Some(Dialog::Conflict(dialog)) => {
                dialog.move_cursor(delta);
                return;
            }
            None => {}
        }
        if let Some(picker) = &mut self.picker {
            picker.move_cursor(delta);
            return;
        }
        let rows = self.task_rows();
        if let Some(panel) = &mut self.panel {
            panel.move_cursor(delta, rows.len());
        }
    }

    /// `Enter` on whatever is up.
    fn submit_overlay(&mut self, now: Instant) {
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
            Some(Dialog::Confirm(_)) | None => {}
        }
        self.picker = None;
        self.panel = None;
        self.sync_context();
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
    fn open_rename(&mut self, empty_stem: bool) {
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

    /// One keystroke into the open prompt.
    ///
    /// The buffer takes **every** key — that is what df-core's editor is for,
    /// and it is why a stray `q` in a rename types a `q` instead of quitting.
    fn prompt_key(&mut self, chord: Chord, now: Instant) {
        let Some(prompt) = &mut self.prompt else { return };
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
        let Some(from) = self
            .tab()
            .cwd
            .dir
            .cursor_entry()
            .map(|entry| entry.path.clone())
        else {
            return Err("nothing under the cursor".to_string());
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
        let direction = if reverse {
            flip(direction)
        } else {
            direction
        };
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
        // A modal card is the nearest thing to the user, and its own `Esc`
        // knows whether it is holding a prompt of its own (PLAN §4.1's extended
        // ladder: dialog → prompt → …). Reached from `Global`'s `Esc` when no
        // overlay is open, and from [`App::overlay_key`] when one is.
        if self.overlay_open() {
            self.close_overlay(Instant::now());
            return;
        }
        if self.keys.is_pending() {
            self.keys.cancel();
            return;
        }
        if self.prompt.is_some() {
            self.cancel_prompt();
            return;
        }
        if self.help.is_some() {
            self.close_help();
            return;
        }
        if self.visual.take().is_some() {
            return;
        }
        let dir = self.dir();
        if dir.selected_count() > 0 {
            dir.clear_selection();
            return;
        }
        if !dir.filter().is_empty() {
            dir.clear_filter();
        }
        // The last rung is `focus = List`, which is where focus already is
        // until the preview pane lands in Phase 3.
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
        self.tabs.active_mut().rescan(&self.scanner, now);
        self.rewatch();
    }

    fn run(&mut self, command: Command, page: usize, now: Instant) {
        use Command as C;
        // Half a page rounds *down* but never to nothing: on a pane too short
        // to have a half, `Ctrl+d` still has to move.
        let half = (page / 2).max(1) as isize;
        let full = page.max(1) as isize;

        match command {
            // ── The cursor ──────────────────────────────────────────────────
            C::CursorUp => self.dir().move_cursor(-1),
            C::CursorDown => self.dir().move_cursor(1),
            C::HalfPageUp => self.dir().move_cursor(-half),
            C::HalfPageDown => self.dir().move_cursor(half),
            C::PageUp => self.dir().move_cursor(-full),
            C::PageDown => self.dir().move_cursor(full),
            C::CursorTop => self.dir().set_cursor(0),
            C::CursorBottom => self.dir().set_cursor(usize::MAX),

            // ── Moving between directories ──────────────────────────────────
            C::Leave => {
                if let Some(parent) = self.tab().cwd.path().parent().map(Path::to_path_buf) {
                    self.navigate(parent, now);
                }
            }
            C::EnterOrPreview => {
                // Directories only, for now: `→` on a *file* focuses the
                // preview pane (PLAN §2.1), and there is no preview yet.
                match self.tab().cwd.dir.cursor_entry() {
                    Some(entry) if entry.is_dir() => {
                        let path = entry.path.clone();
                        self.navigate(path, now);
                    }
                    Some(_) => log::debug!("preview focus arrives in Phase 3"),
                    None => {}
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
                if self.tabs.active_mut().forward(&mgr, sort, &self.scanner, now) {
                    self.visual = None;
                    self.rewatch();
                }
            }
            C::Goto(slot) => self.goto(slot, now),

            // ── The preview, from the list (PLAN §4.1's yazi parity) ────────
            C::SeekPreviewUp => self.preview.seek(false, now),
            C::SeekPreviewDown => self.preview.seek(true, now),

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
            C::Trash => self.open_confirm(ConfirmKind::Trash, now),
            C::DeletePermanently => self.open_confirm(ConfirmKind::Delete, now),
            C::SymlinkAbsolute => self.link(Some(LinkKind::Absolute), now),
            C::SymlinkRelative => self.link(Some(LinkKind::Relative), now),
            C::Hardlink => self.link(None, now),
            C::Create => self.open_prompt(PromptKind::Create),
            C::Rename => self.open_rename(false),
            C::RenameEmptyStem => self.open_rename(true),
            C::Shell => self.open_prompt(PromptKind::Shell),
            C::ShellBlock => self.open_prompt(PromptKind::ShellBlock),
            C::Undo => self.undo(now),
            C::TasksShow => self.toggle_panel(),

            // ── Opening (PLAN §6) ───────────────────────────────────────────
            C::Open => self.open_hovered(now),
            C::OpenInteractive => self.open_picker(now),

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

    /// The `g` chord's bookmarks (PLAN §3's `[goto]` table).
    fn goto(&mut self, slot: u8, now: Instant) {
        let Some(bookmark) = self.config.goto.get(slot as usize) else {
            log::debug!("goto slot {slot} is not in the [goto] table");
            return;
        };
        let path = bookmark.expanded_path();
        // The SFTP bookmarks are real entries in the shipped table and will be
        // real destinations in Phase 6; until the vfs exists, saying so is
        // better than a "no such directory" about a path that is not one.
        if path.contains("://") {
            log::info!("{path} needs the remote vfs, which is a later phase");
            return;
        }
        self.navigate(PathBuf::from(path), now);
    }

    /// Where the open surface's pieces are this frame. Built before the
    /// pointer is looked at, so a click lands on the card rather than on the row
    /// behind it, and reused by the paint so the two cannot disagree.
    fn overlay_geometry(&self, area: egui::Rect, bar_top: f32) -> Option<OverlayGeom> {
        match &self.dialog {
            Some(Dialog::Confirm(confirm)) => {
                return Some(OverlayGeom::Confirm(dialog::confirm_geometry(area, confirm)))
            }
            Some(Dialog::Conflict(conflict)) => {
                return Some(OverlayGeom::Conflict(dialog::conflict_geometry(
                    area, conflict,
                )))
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
        None
    }

    /// A click on a surface. Pressing a button *is* choosing it — the pointer
    /// does not get a two-step "select, then confirm" the keyboard does not
    /// have.
    fn overlay_click(&mut self, control: Control, now: Instant) {
        match control {
            Control::Action(index) => match &mut self.dialog {
                Some(Dialog::Confirm(_)) => {
                    if index == 0 {
                        self.close_overlay(now);
                    } else {
                        self.submit_overlay(now);
                    }
                }
                Some(Dialog::Conflict(conflict)) => {
                    match dialog::ConflictAction::ALL.get(index) {
                        Some(action) => {
                            conflict.set_action(*action);
                            self.submit_overlay(now);
                        }
                        // Past the three answers is the apply-to-all toggle.
                        None => conflict.toggle_apply_all(),
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
                    self.submit_overlay(now);
                    return;
                }
                let rows = self.task_rows();
                if let Some(panel) = &mut self.panel {
                    panel.select(index, rows.len());
                }
            }
            Control::Row(..) | Control::Tab(_) => {}
        }
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
        let page = crate::viewport::visible_rows(
            ui::content_rect(layout.list).height(),
            ui::ROW_HEIGHT,
        );
        self.route_keys(page, now);
        self.which.update(self.keys.which_key_due(), now);

        let layout = ui::layout(area, self.mgr.ratio, self.tabs.len() > 1);
        let list_content = ui::content_rect(layout.list);
        let page = crate::viewport::visible_rows(list_content.height(), ui::ROW_HEIGHT);

        // ── Pointer ─────────────────────────────────────────────────────────
        let (pointer, down, just_pressed) = ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_down(),
                i.pointer.primary_pressed(),
            )
        });
        let scroll_rows = self.tab().cwd.scroll_rows(now);
        let slide = self.tabs.offset(now);
        let tab_count = self.tabs.len();
        let overlay = self.overlay_geometry(area, layout.bar.top());
        let over = pointer.and_then(|p| {
            // A modal surface takes the pointer with the keyboard: nothing
            // behind the scrim is hoverable, so a stray click cannot move the
            // cursor under a question about the row it was on.
            if let Some(overlay) = &overlay {
                return overlay.hit(p).map(|control| (control, p));
            }
            let control = layout
                .strip
                .and_then(|strip| chrome::tab_at(strip, tab_count, p))
                .map(Control::Tab)
                .or_else(|| {
                    ui::row_at(list_content, scroll_rows, self.tab().cwd.dir.len(), p)
                        .map(|index| Control::Row(Column::List, index))
                })?;
            Some((control, p))
        });
        if let Some((control, position)) = over.filter(|_| just_pressed) {
            // Everything happens on mouse-*down*, with the ripple: waiting for
            // the release would put the acknowledgement after the thing it is
            // acknowledging.
            let rect = match control {
                Control::Row(_, index) => {
                    self.dir().set_cursor(index);
                    self.apply_visual();
                    ui::row_rect(list_content, scroll_rows, index)
                }
                Control::Tab(index) => {
                    if self.tabs.switch_to(index, now) {
                        self.tab_changed(now);
                    }
                    layout
                        .strip
                        .map(|strip| chrome::tab_rects(strip, tab_count))
                        .and_then(|rects| rects.get(index).copied())
                        .unwrap_or(egui::Rect::ZERO)
                }
                Control::Action(_) | Control::PanelRow(_) => {
                    let rect = overlay
                        .as_ref()
                        .and_then(|o| o.rect_of(control))
                        .unwrap_or(egui::Rect::ZERO);
                    self.overlay_click(control, now);
                    rect
                }
            };
            self.ripples.spawn(control, position, rect, now);
        }
        self.hovers.tick(
            over.map(|(control, _)| control),
            over.map(|(control, _)| control).filter(|_| down),
            now,
        );
        self.ripples.tick(now);

        // ── Scroll ──────────────────────────────────────────────────────────
        // The scrolloff rule is applied to the *target* row, not to where the
        // rows have animated to, so the maths never chases its own animation.
        let scrolloff = self.mgr.scrolloff;
        let parent_page = crate::viewport::visible_rows(
            ui::content_rect(layout.parent).height(),
            ui::ROW_HEIGHT,
        );
        let tab = self.tabs.active_mut();
        let list_first = crate::viewport::first_visible(
            tab.cwd.first(),
            tab.cwd.dir.cursor(),
            tab.cwd.dir.len(),
            page,
            scrolloff,
        );
        tab.cwd.set_first(list_first, now);
        let cursor = tab.cwd.dir.cursor();
        self.cursor_glow.tick(Some(cursor), None, now);
        // Where a rename popup and the opener picker anchor themselves — the
        // row the cursor is on, as it was actually drawn this frame.
        self.cursor_rect = ui::row_rect(list_content, scroll_rows, cursor);

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
        let hovered = self
            .tab()
            .cwd
            .dir
            .cursor_entry()
            .map(|entry| entry.path.clone());
        self.preview.sync(hovered.as_deref(), target, now);

        // ── The help sheet, and where it has scrolled to ─────────────────────
        // Built before the painter exists, because building it needs `&mut
        // self` and the painter holds the palette.
        let help_view = match self.help {
            Some(mut help) => {
                let rect = chrome::help_rect(area, layout.bar);
                let lines = self.help_lines();
                let total =
                    help::all_rows(&self.keymap, &self.help_stack(), WhenFlags::LIST).len();
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

        // ── Paint ───────────────────────────────────────────────────────────
        let paint = ui::Painting {
            painter: &painter,
            palette: &self.palette,
            theme: &self.theme,
            nerd: self.nerd,
            show_symlink: self.mgr.show_symlink,
            now,
        };

        // The list is the only focusable pane in Phase 1, so it is the one that
        // carries PLAN §2.1's accent rule and tint.
        let list_ground = paint.pane_fill(self.palette.base, true);
        paint.pane(layout.parent, self.palette.mantle, false);
        paint.pane(layout.list, self.palette.base, true);
        paint.pane(layout.preview, self.palette.mantle, false);

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
                linemode: LineMode::None,
                dim: true,
                slow_load: now.duration_since(parent.scan_started) >= LOADING_DELAY,
                offset_x: slide,
                show_selection: false,
                // The clipboard's marks belong to the directory the yank was
                // made in, which is the list — the parent shows where you are,
                // not what you are carrying.
                clip: None,
            });
        }
        paint.listing(ListView {
            pane: layout.list,
            ground: list_ground,
            dir: &self.tab().cwd.dir,
            scroll_rows,
            column: Column::List,
            hovers: &self.hovers,
            ripples: &self.ripples,
            cursor_fill: self.palette.surface1,
            cursor_glow: CursorGlow::Fading(&self.cursor_glow),
            linemode: self.mgr.linemode,
            dim: false,
            slow_load: now.duration_since(self.tab().cwd.scan_started) >= LOADING_DELAY,
            offset_x: slide,
            show_selection: true,
            clip: (!clip_paths.is_empty()).then(|| ClipMark {
                paths: &clip_paths,
                cut: self.clipboard.mode == PasteMode::Cut,
            }),
        });
        crate::preview::preview(&paint, layout.preview, &mut self.preview, ppp, now);

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

        // The help sheet is drawn over the panes but *under* the bar, because
        // the bar is where its filter is typed — an overlay that covered its own
        // input would be asking a question it hid the answer box for.
        if let Some((rect, lines, total, help)) = &help_view {
            chrome::help_overlay(&paint, area, *rect, lines, help, *total);
        }

        // An anchored prompt (`r`, `R`, the conflict rename) floats over the row
        // it is about, so the bar keeps saying where you are underneath it.
        let anchored = self
            .prompt
            .as_ref()
            .filter(|prompt| prompt.kind.anchored());
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
            None if self.overlay_open() => chrome::hint_bar(
                &paint,
                layout.bar,
                &overlay_hints(&self.dialog, self.picker.is_some()),
            ),
            None if self.help.is_some() => chrome::hint_bar(
                &paint,
                layout.bar,
                &[
                    ("↑↓", "move"),
                    ("f", "filter"),
                    ("Esc", "close"),
                ],
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
            (Some(OverlayGeom::Confirm(geometry)), Some(Dialog::Confirm(confirm))) => {
                dialog::paint_confirm(
                    &paint,
                    area,
                    confirm,
                    geometry,
                    &self.hovers,
                    &self.ripples,
                );
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

        // The toast sits above the bar and under the which-key card: a message
        // about what just happened must not cover the answer to the key being
        // held down now.
        self.toasts.paint(&paint, area, layout.bar.top(), now);

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

        // ── The repaint discipline, in one place (PLAN §1) ──────────────────
        // A frame is asked for only while something is actually moving. A
        // pointer parked on a row holds a 1.0 that will be 1.0 again next
        // frame, and `animating()` says so — idle costs zero frames.
        let animating = [
            ("hovers", self.hovers.animating()),
            ("cursor_glow", self.cursor_glow.animating()),
            ("ripples", self.ripples.animating(now)),
            ("tab", self.tab().animating(now)),
            ("tabs", self.tabs.animating(now)),
            ("preview", self.preview.animating(now)),
            ("which", self.which.fading()),
            ("toast", self.toasts.animating(now)),
            (
                "tasks",
                self.panel.as_ref().is_some_and(|p| p.animating(now)),
            ),
        ];
        // DF_FRAME_LOG=1 names whoever is holding the frame rate up — the
        // instrument for the Phase 6 "zero repaints at rest" audit, because a
        // stuck `animating()` source is invisible from outside.
        if frame_log_enabled() {
            let hot: Vec<&str> = animating.iter().filter(|(_, on)| *on).map(|(n, _)| *n).collect();
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
        [
            self.loading_deadline(now),
            card,
            self.preview.next_deadline(now),
            self.toasts.deadline(now),
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
            gfx.window.request_redraw();
            return;
        }
        if !self.logged_first_frame {
            self.logged_first_frame = true;
            log::info!(
                "window mapped {}x{}",
                gfx.surface_config.width,
                gfx.surface_config.height
            );
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
            crate::cli::write_cwd_file(path, self.tab().cwd.path());
        }
        event_loop.exit();
    }
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
fn overlay_hints(dialog: &Option<Dialog>, picker: bool) -> Vec<(&'static str, &'static str)> {
    match dialog {
        Some(Dialog::Confirm(_)) => vec![("Enter / y", "confirm"), ("Esc / n", "cancel"), ("↑↓", "scroll")],
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
                path.parent().map(Path::to_path_buf).unwrap_or_else(fallback),
                name,
            )
        }
        Err(e) => {
            log::warn!("{}: {e}; opening the current directory", path.display());
            (fallback(), None)
        }
    }
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

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gfx) = &mut self.gfx else { return };
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
                    self.pending_keys.push(Press { chord, text });
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
        self.gfx = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            record: Some(OpRecord::Copy { created: Vec::new() }),
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
        );
        assert!(confirm.iter().any(|(k, _)| k.contains("Enter")));
        assert!(confirm.iter().any(|(k, _)| k.contains("Esc")));
        let panel = overlay_hints(&None, false);
        assert!(panel.iter().any(|(k, what)| *k == "x" && *what == "cancel"));
        let picker = overlay_hints(&None, true);
        assert!(picker.iter().any(|(_, what)| *what == "open"));
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
        assert_eq!(nearest_existing(Path::new("/nonexistent/a/b/c")), PathBuf::from("/"));
        let exe = std::env::current_exe().expect("test binary path");
        let parent = exe.parent().expect("a parent").to_path_buf();
        assert_eq!(nearest_existing(&exe.join("gone")), parent);
    }
}
