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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use df_core::config::{Config, LineMode, MgrConfig, SortBy, Theme};
use df_core::fs::{random_seed, Scanner, SortOptions, WatchEvent, Watcher};
use df_core::keymap::{
    Chord, Command, ContextStack, Dispatch, KeymapState, Registry, WhenFlags,
};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::graphics::{Gfx, GfxError};
use crate::hover::Hovers;
use crate::ripple::Ripples;
use crate::tab::Tab;
use crate::theme::Palette;
use crate::ui::{self, Column, Control, CursorGlow, ListView};

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
    tab: Tab,
    /// `--cwd-file`, written on a `q` quit (PLAN §3).
    cwd_file: Option<PathBuf>,
    quit: Option<Quit>,

    // ── Input ───────────────────────────────────────────────────────────────
    /// Keystrokes that arrived since the last frame. Queued rather than acted
    /// on in `window_event` because a command needs the pane geometry — a page
    /// is however many rows are on screen — and that is only known mid-frame.
    pending_keys: Vec<Chord>,
    modifiers: ModifiersState,

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
        let watcher = Watcher::start(notifier);

        let mgr = config.mgr.clone();
        let seed = 0;
        let sort = sort_options(&mgr, seed);
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
            tab,
            cwd_file: args.cwd_file,
            quit: None,
            pending_keys: Vec::new(),
            modifiers: ModifiersState::empty(),
            hovers: Hovers::new(),
            cursor_glow: Hovers::new(),
            ripples: Ripples::new(),
            nerd: false,
        }
    }

    fn sort(&self) -> SortOptions {
        sort_options(&self.mgr, self.seed)
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
        let mut changed = false;
        for update in self.scanner.drain() {
            if self.tab.apply(&update) {
                changed = true;
            }
        }
        if changed {
            // The parent's marker follows the path, and the row it belongs on
            // may only just have arrived in a batch.
            self.tab.sync_parent_cursor();
        }

        for event in self.watcher.drain() {
            changed = true;
            match event {
                WatchEvent::Changed(dir) => self.rescan(&dir, now),
                // The directory we are in stopped existing. Walking up to the
                // nearest ancestor that still does is what a person would do
                // by hand, and leaving the pane showing a listing of a deleted
                // directory is the alternative.
                WatchEvent::Gone(dir) if dir == self.tab.cwd.path() => {
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
        if dir == self.tab.cwd.path() {
            self.tab.cwd.begin_scan(&self.scanner, now);
        }
        if let Some(parent) = &mut self.tab.parent {
            if dir == parent.path() {
                parent.begin_scan(&self.scanner, now);
            }
        }
    }

    fn refresh_all(&mut self, now: Instant) {
        let (mgr, sort) = (self.mgr.clone(), self.sort());
        self.tab.rescan_all(&mgr, sort, &self.scanner, now);
        self.watcher.watch(self.tab.watched());
    }

    fn navigate(&mut self, path: PathBuf, now: Instant) {
        let (mgr, sort) = (self.mgr.clone(), self.sort());
        self.tab.navigate(path, &mgr, sort, &self.scanner, now);
        self.watcher.watch(self.tab.watched());
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
        for chord in std::mem::take(&mut self.pending_keys) {
            match self
                .keymap
                .dispatch(&mut self.keys, &self.context, flags, chord, now)
            {
                Dispatch::Match(command) => self.run(command, page, now),
                // The chord is held. The which-key card that shows what could
                // finish it is a later checkbox; the state it reads is live.
                Dispatch::Pending { .. } => {}
                Dispatch::NoMatch => log::trace!("unbound: {}", chord.label()),
            }
        }
    }

    fn run(&mut self, command: Command, page: usize, now: Instant) {
        use Command as C;
        // Half a page rounds *down* but never to nothing: on a pane too short
        // to have a half, `Ctrl+d` still has to move.
        let half = (page / 2).max(1) as isize;
        let full = page.max(1) as isize;

        match command {
            // ── The cursor ──────────────────────────────────────────────────
            C::CursorUp => self.tab.cwd.dir.move_cursor(-1),
            C::CursorDown => self.tab.cwd.dir.move_cursor(1),
            C::HalfPageUp => self.tab.cwd.dir.move_cursor(-half),
            C::HalfPageDown => self.tab.cwd.dir.move_cursor(half),
            C::PageUp => self.tab.cwd.dir.move_cursor(-full),
            C::PageDown => self.tab.cwd.dir.move_cursor(full),
            C::CursorTop => self.tab.cwd.dir.set_cursor(0),
            C::CursorBottom => self.tab.cwd.dir.set_cursor(usize::MAX),

            // ── Moving between directories ──────────────────────────────────
            C::Leave => {
                if let Some(parent) = self.tab.cwd.path().parent().map(Path::to_path_buf) {
                    self.navigate(parent, now);
                }
            }
            C::EnterOrPreview => {
                // Directories only, for now: `→` on a *file* focuses the
                // preview pane (PLAN §2.1), and there is no preview yet.
                match self.tab.cwd.dir.cursor_entry() {
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
                if self.tab.back(&mgr, sort, &self.scanner, now) {
                    self.watcher.watch(self.tab.watched());
                }
            }
            C::HistoryForward => {
                let (mgr, sort) = (self.mgr.clone(), self.sort());
                if self.tab.forward(&mgr, sort, &self.scanner, now) {
                    self.watcher.watch(self.tab.watched());
                }
            }
            C::Goto(slot) => self.goto(slot, now),

            // ── What is shown ───────────────────────────────────────────────
            C::ToggleHidden => {
                self.mgr.show_hidden = !self.mgr.show_hidden;
                let show = self.mgr.show_hidden;
                self.tab.cwd.dir.set_show_hidden(show);
                if let Some(parent) = &mut self.tab.parent {
                    parent.dir.set_show_hidden(show);
                }
                self.tab.sync_parent_cursor();
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

            // ── Leaving ─────────────────────────────────────────────────────
            C::Quit => self.quit = Some(Quit::WriteCwd),
            C::QuitNoCwdFile => self.quit = Some(Quit::Silent),
            // The Esc ladder (PLAN §4.1) is mostly about state Phase 1 does not
            // have yet. The rung that exists is abandoning a half-typed chord —
            // and dispatch has already done that by the time this runs.
            C::Escape => self.keys.cancel(),

            other => log::debug!("`{}` is not implemented yet", other.id()),
        }
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
        self.tab.cwd.dir.set_sort(sort);
        if let Some(parent) = &mut self.tab.parent {
            parent.dir.set_sort(sort);
        }
        self.tab.sync_parent_cursor();
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
        // routed until the panes have been measured.
        let layout = ui::layout(area, self.mgr.ratio);
        let list_content = ui::content_rect(layout.list);
        let page = crate::viewport::visible_rows(list_content.height(), ui::ROW_HEIGHT);
        self.route_keys(page, now);

        // ── Pointer ─────────────────────────────────────────────────────────
        let (pointer, down, just_pressed) = ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_down(),
                i.pointer.primary_pressed(),
            )
        });
        let scroll_rows = self.tab.cwd.scroll_rows(now);
        let over = pointer.and_then(|p| {
            ui::row_at(list_content, scroll_rows, self.tab.cwd.dir.len(), p)
                .map(|index| (Control::Row(Column::List, index), p))
        });
        if let Some((control, position)) = over.filter(|_| just_pressed) {
            let Control::Row(_, index) = control;
            // The cursor moves on mouse-*down*, with the ripple: waiting for
            // the release would put the acknowledgement after the thing it is
            // acknowledging.
            self.tab.cwd.dir.set_cursor(index);
            let rect = ui::row_rect(list_content, scroll_rows, index);
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
        let list_first = crate::viewport::first_visible(
            self.tab.cwd.first(),
            self.tab.cwd.dir.cursor(),
            self.tab.cwd.dir.len(),
            page,
            self.mgr.scrolloff,
        );
        self.tab.cwd.set_first(list_first, now);
        self.cursor_glow
            .tick(Some(self.tab.cwd.dir.cursor()), None, now);

        let parent_page = crate::viewport::visible_rows(
            ui::content_rect(layout.parent).height(),
            ui::ROW_HEIGHT,
        );
        if let Some(parent) = &mut self.tab.parent {
            let first = crate::viewport::first_visible(
                parent.first(),
                parent.dir.cursor(),
                parent.dir.len(),
                parent_page,
                self.mgr.scrolloff,
            );
            parent.set_first(first, now);
        }

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

        if let Some(parent) = &self.tab.parent {
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
            });
        }
        paint.listing(ListView {
            pane: layout.list,
            ground: list_ground,
            dir: &self.tab.cwd.dir,
            scroll_rows,
            column: Column::List,
            hovers: &self.hovers,
            ripples: &self.ripples,
            cursor_fill: self.palette.surface1,
            cursor_glow: CursorGlow::Fading(&self.cursor_glow),
            linemode: self.mgr.linemode,
            dim: false,
            slow_load: now.duration_since(self.tab.cwd.scan_started) >= LOADING_DELAY,
        });
        paint.preview_placeholder(layout.preview);

        // ── The repaint discipline, in one place (PLAN §1) ──────────────────
        // A frame is asked for only while something is actually moving. A
        // pointer parked on a row holds a 1.0 that will be 1.0 again next
        // frame, and `animating()` says so — idle costs zero frames.
        if self.hovers.animating()
            || self.cursor_glow.animating()
            || self.ripples.animating(now)
            || self.tab.animating(now)
        {
            ui.ctx().request_repaint();
        } else if let Some(due) = self.loading_deadline(now) {
            // The one *scheduled* wake-up: the moment a slow read earns its
            // label. Without it a directory that never sends a batch would stay
            // blank forever, and with a poll it would cost frames for the
            // 99.9% of reads that land immediately.
            ui.ctx().request_repaint_after(due);
        }
    }

    /// How long until a pane has to admit it is loading, if one is about to.
    fn loading_deadline(&self, now: Instant) -> Option<Duration> {
        let panes = std::iter::once(&self.tab.cwd).chain(self.tab.parent.iter());
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
            Some(delay) if delay.is_zero() => gfx.window.request_redraw(),
            Some(delay) if delay < REPAINT_HORIZON => {
                self.repaint_at = Some(Instant::now() + delay);
            }
            _ => {}
        }
    }

    /// Write the cwd-file if this quit calls for one, and say goodbye.
    fn finish(&mut self, event_loop: &ActiveEventLoop) {
        if let (Some(Quit::WriteCwd), Some(path)) = (self.quit, self.cwd_file.as_deref()) {
            crate::cli::write_cwd_file(path, self.tab.cwd.path());
        }
        event_loop.exit();
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
        if response.repaint {
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
                if let Some(chord) = crate::keys::chord(&event, self.modifiers) {
                    self.pending_keys.push(chord);
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

    /// Deleting the directory you are standing in must land somewhere real.
    #[test]
    fn the_nearest_existing_ancestor_is_found() {
        assert_eq!(nearest_existing(Path::new("/nonexistent/a/b/c")), PathBuf::from("/"));
        let exe = std::env::current_exe().expect("test binary path");
        let parent = exe.parent().expect("a parent").to_path_buf();
        assert_eq!(nearest_existing(&exe.join("gone")), parent);
    }
}
