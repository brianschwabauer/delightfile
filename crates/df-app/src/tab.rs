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

use df_core::config::MgrConfig;
use df_core::fs::{DirState, History, ScanUpdate, Scanner, SortOptions};

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
    pub fn new(path: impl Into<PathBuf>, mgr: &MgrConfig, sort: SortOptions, now: Instant) -> Listing {
        let mut dir = DirState::new(path, mgr);
        dir.set_sort(sort);
        Listing {
            dir,
            first: 0,
            scroll: Tween::new(0.0, 0.0, SCROLL_TWEEN, Easing::OutQuint, now),
            wheel_carry: 0.0,
            fresh: true,
            scan_started: now,
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

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
            self.scroll =
                Tween::new(first as f32, first as f32, Duration::ZERO, Easing::Linear, now);
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
    /// The view is moved directly and the **cursor is dragged into it**, which
    /// is the one subtlety here: [`crate::viewport::first_visible`] derives the
    /// view from the cursor every frame, so a wheel that moved only the view
    /// would be undone by the next frame's scrolloff. Dragging the cursor is
    /// also what yazi does, and it means the row you scrolled to is the row the
    /// keyboard is on when you stop.
    ///
    /// Returns whether anything moved.
    pub fn wheel(
        &mut self,
        delta_rows: f32,
        visible: usize,
        scrolloff: usize,
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
        let cursor_row = crate::viewport::cursor_in_view(
            target,
            self.dir.cursor() / columns,
            rows,
            visible,
            scrolloff,
        );
        // Back from a row of the pane to an entry: the same column of that row,
        // so a wheel roll does not also drag the cursor sideways across the
        // grid.
        let column = self.dir.cursor() % columns;
        self.dir.set_cursor(cursor_row * columns + column);
        true
    }

    /// Is the view still moving? The `animating()` half of PLAN §1's idle-cost
    /// rule: a settled list must stop asking for frames.
    pub fn animating(&self, now: Instant) -> bool {
        !self.scroll.finished(now) && (self.scroll.value(now) - self.first as f32).abs() > SCROLL_EPSILON
    }

    pub fn begin_scan(&mut self, scanner: &Scanner, now: Instant) {
        self.dir.begin_scan(scanner);
        self.scan_started = now;
    }
}

/// One tab: the listing, its parent, and the back/forward stacks.
pub struct Tab {
    pub cwd: Listing,
    /// `None` at the filesystem root, which genuinely has no parent — the pane
    /// is drawn empty rather than showing `/` twice.
    pub parent: Option<Listing>,
    pub history: History,
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
        let mut tab = Tab {
            cwd: Listing::new(path.clone(), mgr, sort, now),
            parent: None,
            history: History::new(path),
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
        self.cwd = Listing::new(path, mgr, sort, now);
        self.rescan_all(mgr, sort, scanner, now);
        if leaving.parent() == Some(self.cwd.path()) {
            if let Some(name) = leaving.file_name().map(|n| n.to_string_lossy().into_owned()) {
                self.cwd.dir.cursor_to_name(&name);
            }
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
        self.cwd.begin_scan(scanner, now);
        if let Some(parent) = &mut self.parent {
            parent.begin_scan(scanner, now);
        }
    }

    /// The directories this tab wants watched (PLAN §2: the list and its
    /// parent).
    pub fn watched(&self) -> Vec<PathBuf> {
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

    /// The wheel moves the view and carries the cursor with it, over the
    /// momentum glide rather than the keyboard's shorter step.
    #[test]
    fn the_wheel_scrolls_the_view_and_carries_the_cursor() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        let rows = l.dir.len();
        // `/` has plenty of entries, but the pane in this test is deliberately
        // shorter than the listing so there is something to scroll.
        let visible = 5.min(rows.saturating_sub(1));
        assert!(visible >= 2, "`/` should have more than a couple of entries");

        assert!(l.wheel(3.0, visible, 1, 1, t0));
        assert_eq!(l.first(), 3);
        // The commit is instant; only the drawing lags — and it lags over the
        // wheel's own, longer glide.
        assert_eq!(l.scroll_rows(t0), 0.0);
        assert!(l.animating(t0));
        assert!(!l.animating(t0 + crate::mouse::WHEEL_GLIDE));
        // The cursor came along, and it is inside the window the view landed on.
        let cursor = l.dir.cursor();
        // The window is rows 3..3+visible, and the margin of one keeps the
        // cursor off both of its edges.
        let margin = 1;
        assert!(
            cursor >= 3 + margin && cursor <= 3 + visible - 1 - margin,
            "got {cursor}"
        );

        // Sub-row travel accumulates rather than rounding to nothing.
        let at = t0 + crate::mouse::WHEEL_GLIDE;
        assert!(!l.wheel(0.4, visible, 1, 1, at));
        assert_eq!(l.first(), 3);
        assert!(l.wheel(0.8, visible, 1, 1, at));
        assert_eq!(l.first(), 4);

        // …and it never scrolls past either end.
        for _ in 0..200 {
            l.wheel(-5.0, visible, 1, 1, at);
        }
        assert_eq!(l.first(), 0);
        for _ in 0..400 {
            l.wheel(5.0, visible, 1, 1, at);
        }
        assert_eq!(l.first(), rows - visible);
    }

    /// A listing that fits has nothing to scroll, and the wheel must not move
    /// its cursor as a consolation prize.
    #[test]
    fn the_wheel_does_nothing_to_a_listing_that_fits() {
        let t0 = Instant::now();
        let mut l = listing(t0);
        l.dir.set_cursor(2);
        let visible = l.dir.len() + 10;
        assert!(!l.wheel(4.0, visible, 5, 1, t0));
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
