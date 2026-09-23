//! One browsing session: the directory on screen, its parent, where the cursor
//! is, where the view is, and where the tab has been.
//!
//! PLAN §2 has tabs proper in a later checkbox; this is the *one* tab that
//! exists now, written as a type so that adding the strip later is a `Vec<Tab>`
//! and an index rather than a rewrite of everything that touches a listing.
//!
//! ## Why the view scrolls rather than jumps
//!
//! [`Listing::first`] is an integer row — the scrolloff rule in
//! [`crate::viewport`] is arithmetic over whole rows and stays that way, so the
//! cursor is *always* where it should be the instant the key lands. What
//! animates is only where the rows are *drawn*: a [`Tween`] over the same
//! number, in rows, sampled per frame. State commits instantly and the motion
//! is presentation (`delightful-ui` §5), which also means a held arrow key
//! retargets the tween mid-flight instead of queueing a stack of them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use df_core::config::{MgrConfig, ViewScale};
use df_core::fs::{CursorMemory, DirState, History, ScanUpdate, Scanner, SortOptions};

use crate::motion::{Easing, Tween};

/// How long the view takes to slide to a new scroll position.
///
/// 160 ms, and the number is bounded on both sides. Under ~100 ms the travel is
/// too short to read as motion and the list may as well teleport; over ~200 ms
/// the rows are still moving when the next arrow press arrives and the column
/// feels like it is dragging behind the hand. 160 sits between them, and because
/// the curve is `OutQuint` — front-loaded, so most of the distance is covered in
/// the first third (see [`Easing::OutQuint`]) — the rows are visually in place
/// after about 60 ms and the remaining time is the settle.
const SCROLL_TWEEN: Duration = Duration::from_millis(160);

/// How far a scroll has to travel before it is worth animating, in rows.
///
/// Anything under a row is a sub-pixel correction from a resize, and animating
/// it would mean the list quietly asking for frames after every window change.
const SCROLL_EPSILON: f32 = 0.01;

/// One scrolling directory listing: the model, plus where its view is.
pub struct Listing {
    pub dir: DirState,
    /// The first visible row — an integer, decided by the scrolloff rule.
    first: usize,
    /// Where the rows are *drawn*, in rows, on its way to `first`.
    scroll: Tween,
    /// Wheel travel that has not earned a whole row yet.
    ///
    /// A trackpad reports a few points at a time; without a carry each of those
    /// would round to zero rows and the list would not move at all. See
    /// [`crate::mouse::Fling`], which does the same thing for the preview.
    wheel_carry: f32,
    /// Whether the view has been scrolled away from the cursor, and how many
    /// columns the pane had when it happened.
    ///
    /// A wheel roll (or a drag hanging over the edge) scrolls the *view* and
    /// leaves the cursor exactly where it was — the active row, its preview and
    /// the position counter all stay put even when the row scrolls off screen,
    /// which is what makes the wheel a way to *look* somewhere rather than a
    /// second way to move. That only works if the scrolloff rule stops deriving
    /// the view from the cursor for as long as it lasts.
    ///
    /// **State, not an inference.** This used to hold the cursor position the
    /// wheel left behind and call itself detached for as long as the cursor was
    /// still on it, which got two cases backwards. A `g g` from row 0, or a `G`
    /// on the last row, or an `↑` that wrapped back to where it started, is a
    /// cursor *command* that lands on the same index — and could not take the
    /// view back, because the index had not changed. And a background rescan
    /// that inserted one file above the cursor moved the index without anybody
    /// touching the keyboard, which silently re-attached and snapped the view
    /// out from under a scroll. So detachment is now cleared by the commands
    /// that move the cursor ([`Listing::attach`]) and by nothing else: a
    /// rebuild that keeps the cursor on its file leaves it exactly as it was.
    ///
    /// The column count rides along because [`Listing::first`] counts *rows of
    /// the pane*, and a grid that reflows on a resize changes what a row is —
    /// see [`Listing::reflow`].
    detached: Option<usize>,
    /// Nothing has been drawn yet, so the first scroll position is a *jump*.
    ///
    /// Entering a directory and landing mid-list — `←` back out of a folder,
    /// or opening on a named file — must not slide the rows past you from the
    /// top: those rows were never on screen, and animating between two
    /// different directories' listings is animating between two unrelated
    /// things (`delightful-ui` §5).
    fresh: bool,
    /// When the current scan was asked for, so the pane can tell "still
    /// arriving" from "slow enough to say so" (PLAN §2's first-batch latency).
    pub scan_started: Instant,
}

impl Listing {
    pub fn new(
        path: impl Into<PathBuf>,
        mgr: &MgrConfig,
        sort: SortOptions,
        now: Instant,
    ) -> Listing {
        let mut dir = DirState::new(path, mgr);
        dir.set_sort(sort);
        Listing {
            dir,
            first: 0,
            scroll: Tween::new(0.0, 0.0, SCROLL_TWEEN, Easing::OutQuint, now),
            wheel_carry: 0.0,
            detached: None,
            fresh: true,
            scan_started: now,
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    #[cfg(test)]
    pub fn first(&self) -> usize {
        self.first
    }

    /// Where to draw the rows from, in rows. Fractional mid-slide.
    pub fn scroll_rows(&self, now: Instant) -> f32 {
        self.scroll.value(now)
    }

    /// Move the view. Retargets an in-flight slide from wherever it has got to,
    /// rather than restarting it from the old position — a second arrow press
    /// must not make the list jump backwards before going forwards.
    pub fn set_first(&mut self, first: usize, now: Instant) {
        self.set_first_over(first, SCROLL_TWEEN, now);
    }

    /// The same, with the slide given a length of its own.
    ///
    /// The wheel is the reason: a keyboard step is a *correction* the eye
    /// barely registers, and a wheel roll is travel it has to follow, so the
    /// second one coasts over [`crate::mouse::WHEEL_GLIDE`]. It is still one
    /// eased animation retargeted in flight (PLAN §8), not a second scrolling
    /// mechanism — only the duration differs.
    pub fn set_first_over(&mut self, first: usize, duration: Duration, now: Instant) {
        if self.fresh {
            self.first = first;
            self.scroll = Tween::new(
                first as f32,
                first as f32,
                Duration::ZERO,
                Easing::Linear,
                now,
            );
            // A listing is only "seen" once it has rows in it. Until the first
            // batch lands there is nothing on screen for a slide to move, and
            // the position computed from an empty list is not the one the
            // loaded list will want.
            self.fresh = self.dir.is_empty();
            return;
        }
        if first == self.first {
            return;
        }
        let from = self.scroll.value(now);
        self.first = first;
        self.scroll = Tween::new(from, first as f32, duration, Easing::OutQuint, now);
    }

    /// A wheel roll over this pane (PLAN §7.5's "scroll with momentum").
    ///
    /// The view moves and the **cursor does not**: scrolling is looking, not
    /// selecting, so the active row keeps its preview and its place in the
    /// counter while you read somewhere else in the directory. Because
    /// [`crate::viewport::first_visible`] would otherwise put the view back
    /// under the cursor on the very next frame, the roll *detaches* the view —
    /// see [`Listing::detached`] — until a cursor move takes it back.
    ///
    /// Returns whether anything moved.
    pub fn wheel(
        &mut self,
        delta_rows: f32,
        visible: usize,
        // How many entries share one row of the pane: one in the list, a whole
        // row of tiles in the grid (PLAN §2). Every number below counts *rows
        // of the pane*, so this is the only place the two geometries differ —
        // and passing 1 gives back exactly the arithmetic the list always had.
        columns: usize,
        now: Instant,
    ) -> bool {
        let columns = columns.max(1);
        let rows = self.dir.len().div_ceil(columns);
        if visible == 0 || rows <= visible {
            // Everything fits: there is nothing to scroll, and the cursor
            // belongs to the keyboard.
            self.wheel_carry = 0.0;
            return false;
        }
        self.wheel_carry += delta_rows;
        let whole = self.wheel_carry.trunc();
        self.wheel_carry -= whole;
        if whole == 0.0 {
            return false;
        }
        let max_first = (rows - visible) as f32;
        let target = (self.first as f32 + whole).clamp(0.0, max_first) as usize;
        if target == self.first {
            return false;
        }
        self.set_first_over(target, crate::mouse::WHEEL_GLIDE, now);
        self.detached = Some(columns);
        true
    }

    /// Whether a scroll towards `direction` (negative is up) has anywhere left
    /// to go, counted the way [`Listing::wheel`] counts.
    ///
    /// Asked by a band hanging over the pane's edge, which keeps the frames
    /// coming for as long as it is scrolling — and has to stop the moment the
    /// listing is pinned against its end, or a band held off the bottom of a
    /// finished scroll would run the window at the refresh rate to move nothing
    /// (PLAN §1).
    pub fn can_scroll(&self, direction: f32, visible: usize, columns: usize) -> bool {
        let rows = self.dir.len().div_ceil(columns.max(1));
        if visible == 0 || rows <= visible {
            return false;
        }
        if direction < 0.0 {
            self.first > 0
        } else if direction > 0.0 {
            self.first < rows - visible
        } else {
            false
        }
    }

    /// Whether the view has been scrolled away from the cursor.
    pub fn is_detached(&self) -> bool {
        self.detached.is_some()
    }

    /// Take the view back: the next [`Listing::follow_cursor`] re-anchors it
    /// around wherever the cursor is.
    ///
    /// Called from every path that *sends* the cursor somewhere — each keyboard
    /// cursor command, a click, a find, the start-up placement — including the
    /// ones that discover the cursor was already there. "The cursor did not
    /// move" is not the same fact as "the user did not ask it to", and it is
    /// the asking that ends a scroll.
    pub fn attach(&mut self) {
        self.detached = None;
    }

    /// Re-derive the view for a pane that has changed shape under it.
    ///
    /// [`Listing::first`] is a row of the pane, and in the grid a row is
    /// `columns` entries — so a window resize that reflows three columns into
    /// four leaves `first` meaning something it did not mean a frame ago, and a
    /// detached view (which is the one nothing else will correct) ends up
    /// somewhere unrelated to what was on screen. Re-derived through the entry
    /// that was at the top, which is the thing the eye was actually looking at.
    ///
    /// A no-op while attached: the scrolloff rule puts that view where the
    /// cursor says, every frame, in whatever geometry the pane now has.
    pub fn reflow(&mut self, columns: usize, now: Instant) {
        let columns = columns.max(1);
        let Some(was) = self.detached else { return };
        if was == columns {
            return;
        }
        let top_entry = self.first.saturating_mul(was);
        self.detached = Some(columns);
        // A resize is not a scroll, so this is a jump rather than a slide: the
        // rows have already moved to their new places this frame.
        self.set_first_over(top_entry / columns, Duration::ZERO, now);
    }

    /// Put the view where the cursor says it should be, this frame.
    ///
    /// The scrolloff rule ([`crate::viewport::first_visible`]) applied to the
    /// *target* row, so the maths never chases its own animation — with the one
    /// exception a mouse earns: while the view is detached the cursor is left
    /// off screen on purpose, and all this does is keep the position legal for
    /// a listing that has since got shorter. [`Listing::attach`] is what ends
    /// that, and only a cursor command calls it.
    ///
    /// `cursor_row`, `rows` and `visible` are all counted in *rows of the
    /// pane*: one entry per row in the list, a whole row of tiles in the grid.
    pub fn follow_cursor(
        &mut self,
        cursor_row: usize,
        rows: usize,
        visible: usize,
        scrolloff: usize,
        now: Instant,
    ) {
        if self.is_detached() {
            let max_first = rows.saturating_sub(visible);
            if self.first > max_first {
                self.set_first(max_first, now);
            }
            return;
        }
        let first =
            crate::viewport::first_visible(self.first, cursor_row, rows, visible, scrolloff);
        self.set_first(first, now);
    }

    /// Is the view still moving? The `animating()` half of PLAN §1's idle-cost
    /// rule: a settled list must stop asking for frames.
    pub fn animating(&self, now: Instant) -> bool {
        !self.scroll.finished(now)
            && (self.scroll.value(now) - self.first as f32).abs() > SCROLL_EPSILON
    }

    pub fn begin_scan(&mut self, scanner: &Scanner, now: Instant) {
        self.dir.begin_scan(scanner);
        self.scan_started = now;
    }
}

/// A tab's identity, stable for as long as the tab is open.
///
/// Not its index: tabs are reordered with `{`/`}`, closed from anywhere in the
/// strip, and dragged out into other windows, so "tab 2" names a different tab
/// a keystroke later. Anything that remembers a tab *across a wait* — an
/// asynchronous paste, most of all, which decides its destination when the key
/// is pressed and acts on it when the bytes arrive — has to hold this instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(u64);

impl TabId {
    fn next() -> TabId {
        use std::sync::atomic::{AtomicU64, Ordering};
        // Starts at 1 so no id is ever the default-looking zero.
        static NEXT: AtomicU64 = AtomicU64::new(1);
        TabId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// One tab: the listing, its parent, and the back/forward stacks.
pub struct Tab {
    /// Stable for the life of the tab. See [`TabId`].
    pub id: TabId,
    pub cwd: Listing,
    /// `None` at the filesystem root, which genuinely has no parent — the pane
    /// is drawn empty rather than showing `/` twice.
    pub parent: Option<Listing>,
    pub history: History,
    /// Which row this tab was on, per directory it has left (PLAN §2).
    ///
    /// Going *up* has always landed on the child you came out of; this is the
    /// other three directions. Entering a directory you were in a minute ago,
    /// or `Alt+←` back to one, puts the cursor where you left it rather than on
    /// row 0 — the same "the view does not move under you" rule the reload path
    /// obeys, extended over a navigation. Bounded, and per tab because a tab is
    /// a browsing session: a new tab starts with no memories.
    pub cursors: CursorMemory,
    /// The archive this tab is inside (PLAN §7.3), when it is inside one.
    ///
    /// Per tab, not per app: one tab can be reading a zip while another is in a
    /// real directory, and switching between them switches this with them. When
    /// it is `Some`, [`Tab::cwd`] holds rows built from the tree rather than
    /// from a scan, and its path is a display path that does not exist — see
    /// [`crate::archive`].
    pub archive: Option<crate::archive::Browse>,
    /// The remote service this tab is on (PLAN §7.6), when it is on one.
    ///
    /// Per tab for the same reason the archive is, and mutually exclusive with
    /// it in practice — an archive on a remote service has to be downloaded
    /// first — though nothing here enforces that beyond the fact that entering
    /// one clears the other.
    pub remote: Option<crate::remote::Session>,
    /// The trash, while this tab is browsing it (PLAN §7.4).
    pub trash: Option<crate::trashview::View>,
    /// Where this tab sits on the view-scale ladder ([`ViewScale`]), grid
    /// included.
    ///
    /// Per tab and per session, not per directory: a tab is a browsing
    /// session, and the size you chose to read at is a statement about what
    /// you are doing in it, which does not change because you stepped into a
    /// subfolder. So navigating keeps it, a new tab (and a new window, which
    /// is a new process) starts at `[mgr] view_scale`, and nothing writes it
    /// to the state file.
    pub scale: ViewScale,
    /// The list step `Ctrl+g` comes back to out of the grid: the last one this
    /// tab was at. Never [`ViewScale::Grid`]. While [`Tab::scale`] is a list
    /// step the two are equal.
    pub list_scale: ViewScale,
}

/// Where a new tab starts on the ladder: `(scale, list_scale)`.
///
/// Both from `[mgr] view_scale`. The config refuses `grid` as that default,
/// but [`MgrConfig`] is a plain struct anybody can fill in, and a tab handed
/// the grid anyway still needs a list step for `Ctrl+g` to return to — the
/// bottom of the ladder, which is where the program starts when nobody has
/// said anything.
fn starting_scale(mgr: &MgrConfig) -> (ViewScale, ViewScale) {
    let scale = mgr.view_scale;
    let list = if scale.is_grid() {
        ViewScale::Compact
    } else {
        scale
    };
    (scale, list)
}

/// Which virtual listing a tab is in, if any.
///
/// One question with one answer, asked by everything that has to know whether
/// the rows on screen are files on this machine: the watcher, the preview, the
/// drag source, the operations, and the commands that are inert. Three
/// `is_some()` checks scattered over 8000 lines is how the fourth one gets
/// forgotten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Virtual {
    Archive,
    Remote,
    Trash,
}

impl Tab {
    /// Open `path`, with both panes' scans already queued.
    pub fn open(
        path: impl Into<PathBuf>,
        mgr: &MgrConfig,
        sort: SortOptions,
        scanner: &Scanner,
        now: Instant,
    ) -> Tab {
        let path = path.into();
        let (scale, list_scale) = starting_scale(mgr);
        let mut tab = Tab {
            id: TabId::next(),
            cwd: Listing::new(path.clone(), mgr, sort, now),
            parent: None,
            history: History::new(path),
            cursors: CursorMemory::default(),
            archive: None,
            remote: None,
            trash: None,
            scale,
            list_scale,
        };
        tab.rescan_all(mgr, sort, scanner, now);
        tab
    }

    /// Go to `path`, recording it in the history.
    ///
    /// The cursor lands on the child directory you came out of when there is
    /// one — the `←` case, and the reason [`DirState::cursor_to_name`] exists.
    pub fn navigate(
        &mut self,
        path: impl Into<PathBuf>,
        mgr: &MgrConfig,
        sort: SortOptions,
        scanner: &Scanner,
        now: Instant,
    ) {
        let path = path.into();
        if path == self.cwd.path() {
            return;
        }
        self.history.push(path.clone());
        self.go(path, mgr, sort, scanner, now);
    }

    /// `Alt+←`. Returns whether there was anywhere to go.
    pub fn back(
        &mut self,
        mgr: &MgrConfig,
        sort: SortOptions,
        scanner: &Scanner,
        now: Instant,
    ) -> bool {
        let Some(path) = self.history.go_back().map(Path::to_path_buf) else {
            return false;
        };
        self.go(path, mgr, sort, scanner, now);
        true
    }

    /// `Alt+→`.
    pub fn forward(
        &mut self,
        mgr: &MgrConfig,
        sort: SortOptions,
        scanner: &Scanner,
        now: Instant,
    ) -> bool {
        let Some(path) = self.history.go_forward().map(Path::to_path_buf) else {
            return false;
        };
        self.go(path, mgr, sort, scanner, now);
        true
    }

    /// The move itself, without touching the history — the shared half of
    /// navigate/back/forward.
    fn go(
        &mut self,
        path: PathBuf,
        mgr: &MgrConfig,
        sort: SortOptions,
        scanner: &Scanner,
        now: Instant,
    ) {
        // The name to land the cursor on in the *new* directory: the one we are
        // stepping out of, when this is a step upwards.
        let leaving = self.cwd.path().to_path_buf();
        // Leaving is what makes a memory: the row under the cursor now is the
        // row this directory should show when the tab comes back to it.
        if let Some(name) = self.cwd.dir.cursor_entry().map(|entry| entry.name.clone()) {
            self.cursors.remember(leaving.clone(), name);
        }
        // Still inside the open archive: the rows come out of the tree, and no
        // scan is queued for a directory that is not on the disk.
        match self.archive.as_ref().and_then(|b| b.inner(&path)) {
            Some(inner) => self.show_archive(&inner, mgr, sort, scanner, now),
            None => {
                // Anywhere else is leaving the archive — including the real
                // directory it lives in, which `←` at its root walks out to.
                // Leaving is leaving: the remote session and the trash go too,
                // so a `Alt+←` out of either lands in a plain local listing
                // rather than in a pane that thinks it is still somewhere else.
                self.archive = None;
                self.remote = None;
                self.trash = None;
                self.cwd = Listing::new(path, mgr, sort, now);
                self.rescan_all(mgr, sort, scanner, now);
            }
        }
        // Where the cursor goes in the directory we have arrived in. The
        // step-up rule wins when both apply: walking out of a folder puts you
        // *on that folder*, which is a stronger statement about where you are
        // than what you were looking at here last time.
        //
        // Aimed rather than placed, because the scan that will contain the row
        // has only just been queued — see [`DirState::aim_cursor`].
        let stepped_out_of = (leaving.parent() == Some(self.cwd.path()))
            .then(|| leaving.file_name())
            .flatten()
            .map(|name| name.to_string_lossy().into_owned());
        let land_on = stepped_out_of.or_else(|| self.cursors.recall(self.cwd.path()).cloned());
        if let Some(name) = land_on {
            self.cwd.dir.aim_cursor(name);
        }
    }

    /// Walk into an archive: this tab's list pane becomes its root listing
    /// (PLAN §7.3).
    ///
    /// The history is *not* touched. Back and forward are about directories a
    /// person navigated between, and an archive is a place you step into and
    /// straight back out of; putting `~/dl/src.zip/src` on the history stack
    /// would make `Alt+←` re-open an archive the user had closed.
    pub fn open_archive(
        &mut self,
        browse: crate::archive::Browse,
        mgr: &MgrConfig,
        sort: SortOptions,
        scanner: &Scanner,
        now: Instant,
    ) {
        self.archive = Some(browse);
        self.show_archive("", mgr, sort, scanner, now);
    }

    /// Point both panes at one directory inside the open archive.
    ///
    /// The parent pane is the interesting half: one level in it is another
    /// archive listing, and at the root it is the **real** directory the archive
    /// file lives in — so the column keeps reading as "where you came from" all
    /// the way out, and the archive's own row is the one the cursor sits on.
    fn show_archive(
        &mut self,
        inner: &str,
        mgr: &MgrConfig,
        sort: SortOptions,
        scanner: &Scanner,
        now: Instant,
    ) {
        let Some(browse) = &self.archive else { return };
        // Where the cursor is, by name. Rebuilding the listing is also how the
        // archive view *refreshes*, and a refresh that dropped the cursor to
        // the top would move the row under somebody's hand
        // (`delightful-ui` §8).
        let on = (browse.inner(self.cwd.path()).as_deref() == Some(inner))
            .then(|| self.cwd.dir.cursor_entry().map(|entry| entry.name.clone()))
            .flatten();
        let display = browse.display_path(inner);
        let rows = browse.rows(inner);
        let (parent_path, parent_rows) = match inner.rsplit_once('/') {
            Some((up, _)) => (browse.display_path(up), Some(browse.rows(up))),
            None if inner.is_empty() => (browse.real(), None),
            None => (browse.display_path(""), Some(browse.rows(""))),
        };

        self.cwd = Listing::new(display, mgr, sort, now);
        self.cwd.dir.set_entries(rows);
        if let Some(name) = on {
            self.cwd.dir.cursor_to_name(&name);
        }

        let mut parent = Listing::new(parent_path, mgr, sort, now);
        match parent_rows {
            Some(rows) => parent.dir.set_entries(rows),
            // The one pane in an open archive that is a real directory, and so
            // the one that still gets a scan.
            None => parent.begin_scan(scanner, now),
        }
        self.parent = Some(parent);
        self.sync_parent_cursor();
    }

    // ── The remote pane (PLAN §7.6) ─────────────────────────────────────────

    /// Point this tab at a remote place.
    ///
    /// Returns the [`df_core::vfs::VfsPath`] that needs listing, or `None` when the cache
    /// already had it — which is the whole reason `←` back up a remote tree is
    /// instant. The caller queues the scan, because the [`Vfs`](df_core::vfs)
    /// lives on the app and this type has never been allowed to know about a
    /// worker (it is the same split that keeps [`Scanner`] a parameter).
    ///
    /// The history is *not* touched, for the reason walking into an archive
    /// does not touch it: `Alt+←` is about directories a person navigated
    /// between, and putting `sftp://…` on that stack would make the back button
    /// re-open a connection the user had closed.
    #[must_use]
    pub fn show_remote(
        &mut self,
        at: crate::remote::Session,
        mgr: &MgrConfig,
        sort: SortOptions,
        now: Instant,
    ) -> Option<df_core::vfs::VfsPath> {
        self.archive = None;
        self.trash = None;
        self.remote = Some(at);
        self.refresh_remote(mgr, sort, now)
    }

    /// Rebuild both panes from the session's current place. The shared half of
    /// entering, navigating and refreshing.
    #[must_use]
    pub fn refresh_remote(
        &mut self,
        mgr: &MgrConfig,
        sort: SortOptions,
        now: Instant,
    ) -> Option<df_core::vfs::VfsPath> {
        let session = self.remote.as_ref()?;
        let at = session.at.clone();
        // The row the cursor is on, so a refresh does not move what is under
        // somebody's hand (`delightful-ui` §8).
        let on = (self.cwd.path() == crate::remote::display(&at))
            .then(|| self.cwd.dir.cursor_entry().map(|e| e.name.clone()))
            .flatten();

        let cached = session.cached(&at).cloned();
        let parent_rows = at
            .parent()
            .and_then(|parent| session.cached(&parent).cloned());
        let parent_place = at.parent();
        let origin = session.origin.clone();

        self.cwd = Listing::new(crate::remote::display(&at), mgr, sort, now);
        match &cached {
            Some(rows) => self.cwd.dir.set_entries(rows.clone()),
            // Loading, honestly: the pane's 150 ms hint reads this state, and
            // on a link with real latency it is the state the pane is genuinely
            // in (see `DirState::begin_external`).
            None => self.cwd.dir.begin_external(),
        }
        if let Some(name) = on {
            self.cwd.dir.cursor_to_name(&name);
        }

        // The parent column, from the cache alone. A miss draws it empty rather
        // than spending a second round trip on a column nobody asked to read —
        // and a miss only happens on the first directory of a session, since
        // every step down was listed on the way in.
        let mut parent = match (&parent_place, &parent_rows) {
            (Some(place), _) => Listing::new(crate::remote::display(place), mgr, sort, now),
            // At the service root the parent column is the local directory the
            // session came from, which is also where `←` goes. The column keeps
            // reading as "where you came from" all the way out.
            (None, _) => Listing::new(origin, mgr, sort, now),
        };
        match (&parent_place, parent_rows) {
            (Some(_), Some(rows)) => parent.dir.set_entries(rows),
            (Some(_), None) => parent.dir.set_entries(Vec::new()),
            (None, _) => parent.dir.load_blocking().unwrap_or_default(),
        }
        self.parent = Some(parent);
        self.sync_remote_parent_cursor(&at);
        cached.is_none().then_some(at)
    }

    /// Put the parent column's marker on the remote directory we are inside.
    ///
    /// Not [`Tab::sync_parent_cursor`], because that one reads the cwd's
    /// `file_name()` — and the `file_name()` of `sftp://host/srv` is `srv`
    /// only by accident of the URL happening to look like a path. Asking the
    /// [`df_core::vfs::VfsPath`] is the honest question.
    pub(crate) fn sync_remote_parent_cursor(&mut self, at: &df_core::vfs::VfsPath) {
        if at.parent().is_none() {
            return;
        }
        let name = at.name().to_string();
        if let Some(parent) = &mut self.parent {
            parent.dir.cursor_to_name(&name);
        }
    }

    // ── The trash (PLAN §7.4) ───────────────────────────────────────────────

    /// Point this tab at the trash. The rows are all in hand — a trash listing
    /// is one directory read of `info/` — so there is nothing asynchronous
    /// about it and nothing to return.
    pub fn show_trash(
        &mut self,
        view: crate::trashview::View,
        mgr: &MgrConfig,
        sort: SortOptions,
        now: Instant,
    ) {
        self.archive = None;
        self.remote = None;
        let on = self
            .trash
            .is_some()
            .then(|| self.cwd.dir.cursor_entry().map(|e| e.name.clone()))
            .flatten();
        let rows = crate::trashview::rows(&view.items);
        let origin = view.origin.clone();
        self.trash = Some(view);

        self.cwd = Listing::new(crate::trashview::URL, mgr, sort, now);
        self.cwd.dir.set_entries(rows);
        if let Some(name) = on {
            self.cwd.dir.cursor_to_name(&name);
        }
        // The parent column is the directory `←` goes back to, listed for real
        // — the trash has no parent of its own, and a blank column beside it
        // would waste the one piece of context the view can offer.
        let mut parent = Listing::new(origin, mgr, sort, now);
        parent.dir.load_blocking().unwrap_or_default();
        self.parent = Some(parent);
    }

    /// Which virtual listing this tab is in, if any.
    pub fn virtual_kind(&self) -> Option<Virtual> {
        if self.archive.is_some() {
            Some(Virtual::Archive)
        } else if self.remote.is_some() {
            Some(Virtual::Remote)
        } else if self.trash.is_some() {
            Some(Virtual::Trash)
        } else {
            None
        }
    }

    /// Rebuild the parent listing for the current directory and queue both
    /// scans. Also the "everything changed" path after a watch overflow.
    pub fn rescan_all(
        &mut self,
        mgr: &MgrConfig,
        sort: SortOptions,
        scanner: &Scanner,
        now: Instant,
    ) {
        // Inside an archive there is nothing to re-read: the tree was parsed
        // once and the file behind it has not been reopened. Rebuilding the
        // rows from it is both the refresh and the no-op.
        if let Some(inner) = self.inner_path() {
            self.show_archive(&inner, mgr, sort, scanner, now);
            return;
        }
        // The two other virtual listings are rebuilt by whoever owns their
        // rows — the app, which has the vfs and the trash. A scan of
        // `sftp://…` or `trash://` would be a failed read of a path that is not
        // one, landing a "could not read this directory" over a correct pane.
        if self.remote.is_some() || self.trash.is_some() {
            return;
        }
        self.cwd.begin_scan(scanner, now);
        self.parent = match self.cwd.path().parent() {
            Some(parent) => {
                let mut listing = Listing::new(parent.to_path_buf(), mgr, sort, now);
                listing.begin_scan(scanner, now);
                Some(listing)
            }
            None => None,
        };
    }

    /// What the tab strip calls this tab: the directory's own name, or `/` at
    /// the root, which genuinely has none.
    pub fn title(&self) -> String {
        match self.cwd.path().file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => self.cwd.path().to_string_lossy().into_owned(),
        }
    }

    /// Re-read both panes without rebuilding them.
    ///
    /// The difference from [`Tab::rescan_all`] is the parent listing: this keeps
    /// it, and with it the scroll position and the cursor. It is what a tab gets
    /// when it comes back on screen — a tab that is not active is not watched
    /// (PLAN §2 watches the active tab's directories), so what it is showing may
    /// be minutes old, but it is showing the right *place* and should not jump.
    pub fn rescan(&mut self, scanner: &Scanner, now: Instant) {
        if self.virtual_kind().is_some() {
            // The rows came from a tree, not from a directory; asking the
            // scanner for a path that does not exist would only produce a
            // failed listing where a correct one already is.
            return;
        }
        self.cwd.begin_scan(scanner, now);
        if let Some(parent) = &mut self.parent {
            parent.begin_scan(scanner, now);
        }
    }

    /// Where in the open archive this tab is, or `None` when it is in a real
    /// directory.
    pub fn inner_path(&self) -> Option<String> {
        let browse = self.archive.as_ref()?;
        browse.inner(self.cwd.path())
    }

    /// The directories this tab wants watched (PLAN §2: the list and its
    /// parent).
    pub fn watched(&self) -> Vec<PathBuf> {
        // Inside an archive the only real directory in sight is the one the
        // archive file is in — watching it is how the pane finds out the
        // archive has been deleted out from under it.
        if let Some(browse) = &self.archive {
            return vec![browse.real()];
        }
        // On a remote service the only local directory in sight is the one the
        // session came from — watched so that `←` out of it lands on a listing
        // that is current. The trash watches the same, plus the trash's own
        // `files/` directory, so emptying it from another program is seen.
        if let Some(session) = &self.remote {
            return vec![session.origin.clone()];
        }
        if let Some(view) = &self.trash {
            let mut dirs = vec![view.origin.clone()];
            if let Some(item) = view.items.first() {
                dirs.push(item.trash_root.join("info"));
            }
            return dirs;
        }
        let mut dirs = vec![self.cwd.path().to_path_buf()];
        if let Some(parent) = &self.parent {
            dirs.push(parent.path().to_path_buf());
        }
        dirs
    }

    /// Route one scan update to whichever pane asked for it. Returns whether
    /// anyone wanted it — an update for a directory this tab has left is
    /// dropped, which is what the scan tokens are for.
    pub fn apply(&mut self, update: &ScanUpdate) -> bool {
        if self.cwd.dir.apply(update) {
            return true;
        }
        match &mut self.parent {
            Some(parent) => parent.dir.apply(update),
            None => false,
        }
    }

    /// Put the parent pane's cursor on the directory we are inside, so the
    /// column reads as a path rather than as a second listing. Called after
    /// every scan update, because the row may only just have arrived.
    /// The same, whichever kind of listing this tab is showing.
    ///
    /// The dispatch is the point. [`Tab::sync_parent_cursor`] reads the cwd's
    /// `file_name()`, which for `sftp://host/srv` is `srv` only by accident of
    /// a URL looking like a path — and by accident it stops being one the
    /// moment the URL has a trailing slash or a port in it. Every caller that
    /// runs over *every* tab has to come through here, because a remote tab is
    /// exactly the one it will otherwise get wrong.
    pub fn sync_parent_marker(&mut self) {
        match self.remote.as_ref().map(|session| session.at.clone()) {
            Some(at) => self.sync_remote_parent_cursor(&at),
            None => self.sync_parent_cursor(),
        }
    }

    pub fn sync_parent_cursor(&mut self) {
        let Some(name) = self
            .cwd
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
        else {
            return;
        };
        if let Some(parent) = &mut self.parent {
            parent.dir.cursor_to_name(&name);
        }
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.cwd.animating(now) || self.parent.as_ref().is_some_and(|p| p.animating(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listing that has already been drawn once, which is the state every
    /// test below is about — a *fresh* one snaps rather than slides, and that
    /// is its own test.
    fn listing(now: Instant) -> Listing {
        // `/` always has entries, which is what takes the listing out of its
        // fresh state on the first `set_first`.
        let mut l = Listing::new("/", &MgrConfig::default(), SortOptions::default(), now);
        l.dir.load_blocking().expect("read /");
        l.set_first(0, now);
        l
    }

    /// The commit is instant and only the drawing lags — that is the whole
    /// contract between the scrolloff maths and the animation.
    #[test]
    fn the_view_commits_instantly_and_the_drawing_catches_up() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        l.set_first(10, t0);
        assert_eq!(l.first(), 10);
        assert_eq!(l.scroll_rows(t0), 0.0);
        assert!(l.animating(t0));
        let mid = l.scroll_rows(t0 + Duration::from_millis(60));
        assert!(mid > 0.0 && mid < 10.0, "got {mid}");
        assert!((l.scroll_rows(t0 + SCROLL_TWEEN) - 10.0).abs() < 1e-3);
        assert!(!l.animating(t0 + SCROLL_TWEEN));
    }

    /// A second press mid-slide continues from where the rows are, rather than
    /// snapping back to where they started.
    #[test]
    fn a_second_move_retargets_from_where_the_rows_are() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        l.set_first(10, t0);
        let at = t0 + Duration::from_millis(60);
        let mid = l.scroll_rows(at);
        l.set_first(11, at);
        assert_eq!(l.scroll_rows(at), mid, "the rows must not jump on retarget");
        assert!((l.scroll_rows(at + SCROLL_TWEEN) - 11.0).abs() < 1e-3);
    }

    /// Setting the same position again is not a new animation — otherwise every
    /// frame of a held key would restart the slide and it would never arrive.
    #[test]
    fn setting_the_same_position_does_nothing() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        l.set_first(4, t0);
        let at = t0 + Duration::from_millis(80);
        let mid = l.scroll_rows(at);
        l.set_first(4, at);
        assert_eq!(l.scroll_rows(at), mid);
        assert!((l.scroll_rows(at + SCROLL_TWEEN) - 4.0).abs() < 1e-3);
    }

    /// The wheel moves the view and **leaves the cursor**, over the momentum
    /// glide rather than the keyboard's shorter step.
    #[test]
    fn the_wheel_scrolls_the_view_and_leaves_the_cursor() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        let rows = l.dir.len();
        // `/` has plenty of entries, but the pane in this test is deliberately
        // shorter than the listing so there is something to scroll.
        let visible = 5.min(rows.saturating_sub(1));
        assert!(
            visible >= 2,
            "`/` should have more than a couple of entries"
        );

        assert!(l.wheel(3.0, visible, 1, t0));
        assert_eq!(l.first(), 3);
        // The commit is instant; only the drawing lags — and it lags over the
        // wheel's own, longer glide.
        assert_eq!(l.scroll_rows(t0), 0.0);
        assert!(l.animating(t0));
        assert!(!l.animating(t0 + crate::mouse::WHEEL_GLIDE));
        // The cursor stayed exactly where it was, off the top of the window:
        // scrolling is looking, not selecting.
        assert_eq!(l.dir.cursor(), 0);
        assert!(l.is_detached());
        // …and no number of frames drags the view back to it.
        for _ in 0..3 {
            l.follow_cursor(l.dir.cursor(), rows, visible, 1, t0);
            assert_eq!(l.first(), 3);
        }

        // Sub-row travel accumulates rather than rounding to nothing.
        let at = t0 + crate::mouse::WHEEL_GLIDE;
        assert!(!l.wheel(0.4, visible, 1, at));
        assert_eq!(l.first(), 3);
        assert!(l.wheel(0.8, visible, 1, at));
        assert_eq!(l.first(), 4);

        // …and it never scrolls past either end.
        for _ in 0..200 {
            l.wheel(-5.0, visible, 1, at);
        }
        assert_eq!(l.first(), 0);
        for _ in 0..400 {
            l.wheel(5.0, visible, 1, at);
        }
        assert_eq!(l.first(), rows - visible);
    }

    /// A band hanging over the edge asks whether there is anywhere left to
    /// scroll, and the answer is "no" at each end, towards that end — which is
    /// what lets it stop asking for frames there.
    #[test]
    fn a_listing_knows_when_a_scroll_has_nowhere_to_go() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        let rows = l.dir.len();
        let visible = 5.min(rows.saturating_sub(1));
        assert!(visible >= 2);
        // At the top: down has room, up does not, and still has none.
        assert!(l.can_scroll(1.0, visible, 1));
        assert!(!l.can_scroll(-1.0, visible, 1));
        assert!(!l.can_scroll(0.0, visible, 1));
        // At the bottom, the other way round.
        for _ in 0..400 {
            l.wheel(5.0, visible, 1, t0);
        }
        assert!(!l.can_scroll(1.0, visible, 1));
        assert!(l.can_scroll(-1.0, visible, 1));
        // A listing that fits has nowhere to go at all.
        assert!(!l.can_scroll(1.0, rows, 1));
        assert!(!l.can_scroll(-1.0, rows + 3, 1));
    }

    /// The other half of the rule: the next key takes the view back, moving the
    /// cursor from where it *was* and re-anchoring the window around it.
    #[test]
    fn the_next_cursor_move_re_anchors_the_view() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        let rows = l.dir.len();
        let visible = 5.min(rows.saturating_sub(1));
        assert!(visible >= 2);

        l.dir.set_cursor(1);
        l.follow_cursor(l.dir.cursor(), rows, visible, 1, t0);
        let parked = l.first();

        assert!(l.wheel(3.0, visible, 1, t0));
        assert_ne!(l.first(), parked);
        assert_eq!(l.dir.cursor(), 1);

        // `↓`: the cursor moves from row 1, not from the top of the window the
        // wheel left behind…
        l.dir.move_cursor(1);
        l.attach();
        assert_eq!(l.dir.cursor(), 2);
        assert!(!l.is_detached());
        // …and the view comes back to a position that shows it.
        l.follow_cursor(l.dir.cursor(), rows, visible, 1, t0);
        assert!(
            l.first() <= 2 && 2 < l.first() + visible,
            "the cursor must be back on screen, first = {}",
            l.first()
        );
    }

    /// A click is a cursor move like any other: it takes the view back too.
    #[test]
    fn a_click_after_a_wheel_takes_the_cursor_and_the_view_with_it() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        let rows = l.dir.len();
        let visible = 5.min(rows.saturating_sub(1));
        assert!(visible >= 2);

        assert!(l.wheel(4.0, visible, 1, t0));
        assert!(l.is_detached());
        // The row under the pointer, which is a row of the *scrolled* window.
        let clicked = l.first() + 1;
        l.dir.set_cursor(clicked);
        l.attach();
        assert!(!l.is_detached());
        l.follow_cursor(l.dir.cursor(), rows, visible, 1, t0);
        assert_eq!(l.dir.cursor(), clicked);
        assert!(l.first() <= clicked && clicked < l.first() + visible);
    }

    /// The two cases the old index comparison got wrong: a cursor command that
    /// lands where the cursor already was still takes the view back, and a
    /// rebuild that moves the cursor's index does not.
    #[test]
    fn detachment_is_state_rather_than_an_index_comparison() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        let rows = l.dir.len();
        let visible = 5.min(rows.saturating_sub(1));
        assert!(visible >= 2);

        // `g g` on row 0: the cursor does not move, and the view must still
        // come back to it.
        assert!(l.wheel(3.0, visible, 1, t0));
        assert!(l.is_detached());
        assert_eq!(l.dir.cursor(), 0);
        l.dir.set_cursor(0);
        l.attach();
        assert!(!l.is_detached());
        l.follow_cursor(l.dir.cursor(), rows, visible, 1, t0);
        assert_eq!(l.first(), 0, "`g g` re-anchors even from row 0");

        // …and the other way round: a rescan that shifted the cursor's index
        // under a scroll leaves the scroll where it is.
        assert!(l.wheel(3.0, visible, 1, t0));
        let parked = l.first();
        l.dir.set_cursor(2); // as a rebuild would, without a key being pressed
        assert!(l.is_detached());
        l.follow_cursor(l.dir.cursor(), rows, visible, 1, t0);
        assert_eq!(l.first(), parked);
    }

    /// A grid that reflows on a resize keeps looking at the same entry, rather
    /// than reading its old row number against a new column count.
    #[test]
    fn a_reflow_re_derives_a_detached_view() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        let entries = l.dir.len();
        // Three columns, a pane four rows deep, scrolled two rows down: the
        // top-left tile is entry 6.
        let visible = 4;
        assert!(entries.div_ceil(3) > visible, "`/` is big enough");
        assert!(l.wheel(2.0, visible, 3, t0));
        assert_eq!(l.first(), 2);

        // Widen to four columns: entry 6 is now on row 1.
        l.reflow(4, t0);
        assert_eq!(l.first(), 1);
        // …and a second call with the same count is not a second jump.
        l.reflow(4, t0);
        assert_eq!(l.first(), 1);

        // An attached view is derived from the cursor every frame, so it has
        // nothing to re-derive.
        l.attach();
        l.reflow(2, t0);
        assert_eq!(l.first(), 1);
    }

    /// A detached view must stay legal when the listing shrinks under it —
    /// a directory whose files were deleted elsewhere, mid-scroll.
    #[test]
    fn a_detached_view_is_clamped_to_a_listing_that_shrank() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        let rows = l.dir.len();
        let visible = 5.min(rows.saturating_sub(1));
        assert!(visible >= 2);
        assert!(l.wheel(4.0, visible, 1, t0));
        // Only two rows left: the view has nowhere legal to be but the top.
        l.follow_cursor(l.dir.cursor(), 2, visible, 1, t0);
        assert_eq!(l.first(), 0);
    }

    /// A listing that fits has nothing to scroll, and the wheel must not move
    /// its cursor as a consolation prize.
    #[test]
    fn the_wheel_does_nothing_to_a_listing_that_fits() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        l.dir.set_cursor(2);
        let visible = l.dir.len() + 10;
        assert!(!l.wheel(4.0, visible, 1, t0));
        assert_eq!(l.first(), 0);
        assert_eq!(l.dir.cursor(), 2);
    }

    /// A new directory's rows were never on screen, so the first position is a
    /// snap — and a settled list must not ask for frames.
    #[test]
    fn the_first_position_of_a_new_listing_snaps() {
        let t0 = Instant::now();
        let mut l = Listing::new("/", &MgrConfig::default(), SortOptions::default(), t0);
        l.dir.load_blocking().expect("read /");
        l.set_first(30, t0);
        assert_eq!(l.scroll_rows(t0), 30.0);
        assert!(!l.animating(t0));
        // …and the *second* one animates, because those rows have been seen.
        l.set_first(31, t0);
        assert!(l.animating(t0));
    }

    /// The remote pane's whole state machine, without a network: entering asks
    /// for a listing, the cache answers the second time, and the parent column
    /// is filled from the cache rather than from a second round trip
    /// (PLAN §7.6).
    #[test]
    fn a_remote_pane_lists_once_and_reads_the_cache_after() {
        use df_core::vfs::VfsPath;
        let t0 = Instant::now();
        let (mgr, sort) = (MgrConfig::default(), SortOptions::default());
        let (scale, list_scale) = starting_scale(&mgr);
        let mut tab = Tab {
            id: TabId::next(),
            cwd: Listing::new("/tmp", &mgr, sort, t0),
            parent: None,
            history: History::new("/tmp"),
            cursors: CursorMemory::default(),
            archive: None,
            remote: None,
            trash: None,
            scale,
            list_scale,
        };

        let root = VfsPath::new("showandtour1", "");
        let session = crate::remote::Session::new(root.clone(), PathBuf::from("/tmp"));
        // Nothing cached, so the caller is told to go and list it — and the
        // pane is honestly `Loading` while it does, which is the state the
        // 150 ms hint reads.
        assert_eq!(tab.show_remote(session, &mgr, sort, t0), Some(root.clone()));
        assert_eq!(tab.cwd.path(), Path::new("sftp://showandtour1"));
        assert_eq!(tab.cwd.dir.state(), df_core::fs::LoadState::Loading);
        assert_eq!(tab.virtual_kind(), Some(Virtual::Remote));
        // At the service root the parent column is the local directory the
        // session came from, so the column still reads as "where you came
        // from" all the way out.
        assert_eq!(
            tab.parent.as_ref().map(|p| p.path()),
            Some(Path::new("/tmp"))
        );
        // …and that is the only directory worth watching while we are away.
        assert_eq!(tab.watched(), vec![PathBuf::from("/tmp")]);

        // The listing arrives.
        let rows = vec![row("srv", true), row("readme.md", false)];
        if let Some(session) = &mut tab.remote {
            session.pending = Some((df_core::vfs::VfsToken(1), root.clone()));
        }
        tab.cwd.dir.begin_external();
        tab.cwd.dir.extend_external(rows.clone());
        tab.cwd.dir.finish_external();
        if let Some(session) = &mut tab.remote {
            session.store(&root, rows);
        }
        assert_eq!(tab.cwd.dir.len(), 2);
        assert_eq!(tab.cwd.dir.state(), df_core::fs::LoadState::Loaded);

        // Step into `srv`. Unlisted, so it needs a round trip — but the parent
        // column is free, because its rows were in hand a moment ago.
        let srv = root.join("srv");
        let session = crate::remote::Session::new(srv.clone(), PathBuf::from("/tmp"));
        let mut session = session;
        session.store(&root, tab.cwd.dir.entries().to_vec());
        assert_eq!(tab.show_remote(session, &mgr, sort, t0), Some(srv));
        let parent = tab.parent.as_ref().expect("a parent column");
        assert_eq!(parent.path(), Path::new("sftp://showandtour1"));
        assert_eq!(parent.dir.len(), 2);
        // …with its marker on the directory we walked into, which is what
        // makes the column read as a path rather than as a second listing.
        assert_eq!(
            parent.dir.cursor_entry().map(|e| e.name.as_str()),
            Some("srv")
        );

        // Back out to the root: cached, so no listing is asked for at all.
        let mut session = crate::remote::Session::new(root.clone(), PathBuf::from("/tmp"));
        session.store(&root, vec![row("srv", true), row("readme.md", false)]);
        assert_eq!(tab.show_remote(session, &mgr, sort, t0), None);
        assert_eq!(tab.cwd.dir.len(), 2);
        assert_eq!(tab.cwd.dir.state(), df_core::fs::LoadState::Loaded);

        // …and an operation that changes a directory takes it back out, so the
        // next visit is a real read.
        if let Some(session) = &mut tab.remote {
            session.invalidate(&root);
        }
        assert_eq!(tab.refresh_remote(&mgr, sort, t0), Some(root));
    }

    /// Walking into the trash and back out again (PLAN §7.4). The pane is not a
    /// directory, and `←` returns to the one the user came from.
    #[test]
    fn the_trash_is_a_listing_you_can_walk_back_out_of() {
        let t0 = Instant::now();
        let (mgr, sort) = (MgrConfig::default(), SortOptions::default());
        let (scale, list_scale) = starting_scale(&mgr);
        let mut tab = Tab {
            id: TabId::next(),
            cwd: Listing::new("/tmp", &mgr, sort, t0),
            parent: None,
            history: History::new("/tmp"),
            cursors: CursorMemory::default(),
            archive: None,
            remote: None,
            trash: None,
            scale,
            list_scale,
        };
        let view = crate::trashview::View {
            items: vec![df_core::ops::TrashedItem {
                trash_root: PathBuf::from("/tmp/Trash"),
                name: std::ffi::OsString::from("a.txt"),
                original: PathBuf::from("/tmp/a.txt"),
                deleted_at: "2026-08-30T09:15:00".to_string(),
            }],
            origin: PathBuf::from("/tmp"),
        };
        tab.show_trash(view, &mgr, sort, t0);
        assert_eq!(tab.virtual_kind(), Some(Virtual::Trash));
        assert_eq!(tab.cwd.path(), Path::new(crate::trashview::URL));
        assert_eq!(tab.cwd.dir.len(), 1);
        // The parent column is the real directory `←` goes back to.
        assert_eq!(
            tab.parent.as_ref().map(|p| p.path()),
            Some(Path::new("/tmp"))
        );

        // Navigating anywhere real leaves the trash behind entirely — a pane
        // that still thought it was in the trash would offer restore on files.
        tab.navigate(
            "/tmp",
            &mgr,
            sort,
            &Scanner::start(df_core::fs::no_notifier()),
            t0,
        );
        assert_eq!(tab.virtual_kind(), None);
        assert!(tab.trash.is_none());
    }

    /// A row from nowhere, for the tests above.
    fn row(name: &str, is_dir: bool) -> df_core::fs::Entry {
        df_core::fs::Entry {
            is_hidden: false,
            name: name.to_string(),
            path: PathBuf::from(format!("sftp://showandtour1/{name}")),
            kind: if is_dir {
                df_core::fs::Kind::Dir
            } else {
                df_core::fs::Kind::File
            },
            len: 0,
            mtime: None,
            btime: None,
            mode: 0o100_644,
            uid: 0,
            gid: 0,
            mime: "text/plain",
            file_kind: if is_dir {
                df_core::fs::FileKind::Directory
            } else {
                df_core::fs::FileKind::Text
            },
        }
    }

    /// PLAN §2's per-directory cursor, end to end against a real scanner —
    /// which is the only way to test it, because every one of these placements
    /// happens while the directory read is still in flight.
    #[test]
    fn a_tab_remembers_where_the_cursor_was_in_each_directory() {
        let tree = std::env::temp_dir().join(format!("df-cursor-memory-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tree);
        for dir in ["one", "two"] {
            std::fs::create_dir_all(tree.join(dir)).expect("make the fixture");
            for name in ["a.txt", "b.txt", "c.txt"] {
                std::fs::write(tree.join(dir).join(name), b"x").expect("write the fixture");
            }
        }

        let scanner = Scanner::start(df_core::fs::no_notifier());
        let (mgr, sort) = (MgrConfig::default(), SortOptions::default());
        let t0 = Instant::now();
        let mut tab = Tab::open(tree.clone(), &mgr, sort, &scanner, t0);
        settle(&mut tab, &scanner);

        // Into `one`, and down to the third file.
        tab.navigate(tree.join("one"), &mgr, sort, &scanner, t0);
        settle(&mut tab, &scanner);
        tab.cwd.dir.cursor_to_name("c.txt");

        // Out again. The step-up rule puts the cursor on the folder we left,
        // even though the directory read had not returned when it was asked
        // for.
        tab.navigate(tree.clone(), &mgr, sort, &scanner, t0);
        settle(&mut tab, &scanner);
        assert_eq!(
            tab.cwd.dir.cursor_entry().map(|e| e.name.as_str()),
            Some("one")
        );

        // …and back in: the row we were on, not row 0.
        tab.navigate(tree.join("one"), &mgr, sort, &scanner, t0);
        settle(&mut tab, &scanner);
        assert_eq!(
            tab.cwd.dir.cursor_entry().map(|e| e.name.as_str()),
            Some("c.txt")
        );

        // The step-up rule still wins over the memory when both apply: we were
        // last on `one` up there, and we are coming out of `two`.
        tab.navigate(tree.join("two"), &mgr, sort, &scanner, t0);
        settle(&mut tab, &scanner);
        tab.navigate(tree.clone(), &mgr, sort, &scanner, t0);
        settle(&mut tab, &scanner);
        assert_eq!(
            tab.cwd.dir.cursor_entry().map(|e| e.name.as_str()),
            Some("two")
        );

        // `Alt+←` is a navigation like any other, so it remembers too.
        assert!(tab.back(&mgr, sort, &scanner, t0));
        settle(&mut tab, &scanner);
        assert_eq!(tab.cwd.path(), tree.join("two"));
        assert!(tab.back(&mgr, sort, &scanner, t0));
        settle(&mut tab, &scanner);
        assert!(tab.forward(&mgr, sort, &scanner, t0));
        settle(&mut tab, &scanner);
        assert_eq!(tab.cwd.path(), tree.join("two"));
        assert_eq!(
            tab.cwd.dir.cursor_entry().map(|e| e.name.as_str()),
            Some("a.txt")
        );

        let _ = std::fs::remove_dir_all(&tree);
    }

    /// Drive the scans to completion the way `App::poll_workers` does.
    fn settle(tab: &mut Tab, scanner: &Scanner) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while tab.cwd.dir.state() != df_core::fs::LoadState::Loaded && Instant::now() < deadline {
            for update in scanner.drain() {
                tab.apply(&update);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // One more pass, so the parent column's batches are not left in the
        // channel for the next navigation to pick up.
        for update in scanner.drain() {
            tab.apply(&update);
        }
    }

    /// A listing whose scan has not landed stays fresh: the position computed
    /// from an empty list is not the one the loaded list will want, and sliding
    /// to it would be sliding to the wrong place.
    #[test]
    fn an_empty_listing_stays_ready_to_snap() {
        let t0 = Instant::now();
        let mut l = Listing::new("/", &MgrConfig::default(), SortOptions::default(), t0);
        l.set_first(0, t0);
        l.dir.load_blocking().expect("read /");
        l.set_first(12, t0);
        assert_eq!(l.scroll_rows(t0), 12.0);
        assert!(!l.animating(t0));
    }
}
