//! The directory model: reading a directory, watching it, and putting it in
//! order.
//!
//! Reads are async and results arrive on a channel, because a cold NFS mount or
//! a directory of 200k files must never be something the event loop waits on.
//! Sorting, `sort_dir_first`, the hidden-file toggle, filtering and find are
//! pure functions over the entry list (PLAN §2, §9) so they can be tested
//! against a fixture tree of gnarly names rather than by looking at the screen.
//! Later this is also where the archive and SFTP virtual filesystems attach —
//! one entry-list shape, several sources.
//!
//! ## The shape
//!
//! ```text
//!   scan  ──batches──▶  DirState.entries   (scan order, never reordered)
//!                              │
//!                    sort::sort_order      (a permutation)
//!                              │
//!            filter::filter_indices        (hidden toggle + `f` query)
//!                              │
//!                        DirState.view     ──▶  the rows the pane draws
//! ```
//!
//! Entries are stored in the order the filesystem handed them over and are
//! **never** permuted. Everything the user can change — the sort mode, the
//! direction, `dir_first`, `.`, `f` — only recomputes `view`, a `Vec<usize>`.
//! Three consequences, all of them the point:
//!
//! - Changing the sort of a 200k-entry directory moves 200k `usize`s, not 200k
//!   strings and paths.
//! - The old and the new `view` together *are* the FLIP animation PLAN §8 wants
//!   for a re-sort: every row's before and after position, for free.
//! - The cursor and the selection are held **by name**, not by index, so a
//!   rescan that inserts a file above the cursor does not move the cursor, and
//!   a selection survives a sort change. An index-based cursor is the bug where
//!   a background copy finishing scrolls the list out from under you.
//!
//! ## Generations
//!
//! Every scan carries a [`ScanToken`]; [`DirState`] ignores updates that do not
//! carry the token it is waiting for. Arrowing quickly through directories
//! leaves stale scans in flight and their batches must not land in the pane
//! that has moved on — this is the whole of that guarantee, and it is one
//! comparison.

mod entry;
mod filter;
mod history;
mod inotify;
mod memory;
pub mod mime;
pub mod owner;
mod scan;
mod sort;
mod watch;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub use entry::{Entry, Kind, LinkTarget};
pub use filter::{
    filter_indices, find_from, is_case_sensitive, match_name, FindDirection, Matched, Span,
};
pub use history::{History, HISTORY_LIMIT};
pub use memory::{CursorMemory, Recent, CURSOR_MEMORY};
pub use scan::{
    no_notifier, scan_blocking, Notifier, ScanToken, ScanUpdate, Scanner, BATCH, FIRST_BATCH,
    SCAN_WORKERS,
};
pub use sort::{alphabetical_cmp, natural_cmp, random_seed, sort_entries, sort_order, SortOptions};
pub use watch::{WatchEvent, Watcher, DEBOUNCE};

use crate::config::MgrConfig;
use crate::DfError;

/// How many entries a still-loading listing re-sorts on every batch.
///
/// Sorting as the batches land is what makes a directory readable while it is
/// still being read — but it is a full sort per batch, so the total work grows
/// with the *square* of the batch count. At 10k entries that is ~20 sorts of an
/// average 5k rows: a couple of milliseconds, spread over the load. At 200k it
/// would be ~400 sorts averaging 100k rows, which is seconds of the app thread
/// and the exact stall this design exists to avoid.
///
/// So past this many entries the view stops being rebuilt mid-scan and is
/// rebuilt once when the scan finishes. What that costs: in a directory of more
/// than ten thousand files, rows past the ten-thousandth appear when the scan
/// completes rather than as they arrive. What it buys: the first ten thousand
/// are sorted and scrollable the whole time, and the pane never freezes. Ten
/// thousand is roughly a hundred screenfuls — far past where anyone is reading
/// rather than searching (`f`, `s`, `z`), which is the honest reason it is safe
/// to stop.
pub const STREAM_SORT_LIMIT: usize = 10_000;

/// How a listing is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadState {
    /// Nothing asked for yet.
    Idle,
    /// A scan is in flight. Entries may already be present — the first batch
    /// paints while the rest is still arriving, which is the whole point of
    /// batching (PLAN §2's "yazi feels instant").
    Loading,
    /// Every entry has arrived.
    Loaded,
    /// The directory could not be read. The pane draws the reason rather than
    /// an empty list, because "no permission" and "empty" look identical
    /// otherwise.
    Failed,
}

/// One loaded directory: its entries, how they are ordered and filtered, where
/// the cursor is, and what is selected.
#[derive(Debug, Clone)]
pub struct DirState {
    path: PathBuf,
    entries: Vec<Entry>,
    /// Indices into `entries`, in draw order, after sort + hidden + filter.
    view: Vec<usize>,
    /// Highlight spans per visible row, parallel to `view`. Kept beside the
    /// view rather than on the entry because they belong to the *query*, not to
    /// the file — the same entry has different spans as you type.
    spans: Vec<Vec<Span>>,
    cursor: usize,
    /// The name under the cursor, so a reload can put it back on the same file
    /// even though its index changed.
    cursor_name: Option<String>,
    /// A name the cursor has been *aimed* at that is not in the listing yet.
    ///
    /// A directory read is asynchronous, so every caller that knows where the
    /// cursor belongs in a directory it is about to open — the child you
    /// stepped out of, the row this tab was last on (`Recent`) — knows it
    /// before there is a row to point at. Held here and honoured by the next
    /// [`DirState::rebuild`], which is the moment the row exists.
    wanted_cursor: Option<String>,
    /// Selected file names. Names rather than paths because a `DirState` is one
    /// directory, and a `BTreeSet` rather than a hash set because paste order
    /// has to be deterministic — an operation that processes files in a
    /// different order each run is not one you can reason about.
    selected: BTreeSet<String>,
    sort: SortOptions,
    show_hidden: bool,
    filter: String,
    token: Option<ScanToken>,
    state: LoadState,
    /// Bumped every time `view` is rebuilt, so a UI can tell "the same list,
    /// redrawn" from "a different list" without comparing it.
    generation: u64,
    error: Option<String>,
}

impl DirState {
    /// An empty listing for `path`, configured from `mgr` (PLAN §2's defaults).
    pub fn new(path: impl Into<PathBuf>, mgr: &MgrConfig) -> DirState {
        DirState {
            path: path.into(),
            entries: Vec::new(),
            view: Vec::new(),
            spans: Vec::new(),
            cursor: 0,
            cursor_name: None,
            wanted_cursor: None,
            selected: BTreeSet::new(),
            sort: SortOptions::from_config(mgr),
            show_hidden: mgr.show_hidden,
            filter: String::new(),
            token: None,
            state: LoadState::Idle,
            generation: 0,
            error: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state(&self) -> LoadState {
        self.state
    }

    /// Why the load failed, if it did.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Every entry, in scan order. Rarely what a caller wants — see
    /// [`DirState::rows`].
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The indices of the visible rows, in draw order.
    pub fn view(&self) -> &[usize] {
        &self.view
    }

    /// How many rows are on screen (after the hidden toggle and the filter).
    pub fn len(&self) -> usize {
        self.view.len()
    }

    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    /// How many entries the directory has, before filtering — so a pane can say
    /// "3 of 412" rather than pretending the other 409 are not there.
    pub fn total(&self) -> usize {
        self.entries.len()
    }

    /// The visible rows with their highlight spans, in draw order.
    pub fn rows(&self) -> impl Iterator<Item = (&Entry, &[Span])> + '_ {
        self.view
            .iter()
            .zip(&self.spans)
            .filter_map(move |(index, spans)| Some((self.entries.get(*index)?, spans.as_slice())))
    }

    /// The entry at a *view* position.
    pub fn row(&self, position: usize) -> Option<&Entry> {
        self.entries.get(*self.view.get(position)?)
    }

    /// The highlight spans at a view position.
    pub fn row_spans(&self, position: usize) -> &[Span] {
        self.spans.get(position).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Where the cursor is, as a view position. Always in range when the list
    /// is non-empty; meaningless (0) when it is empty.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The file the cursor is on.
    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.row(self.cursor)
    }

    /// Put the cursor at a view position, clamped.
    pub fn set_cursor(&mut self, position: usize) {
        self.cursor = position.min(self.view.len().saturating_sub(1));
        self.wanted_cursor = None;
        self.remember_cursor();
    }

    /// Move the cursor by `delta` rows, clamped at both ends.
    ///
    /// Clamped rather than wrapping, because this is what a *page* is measured
    /// in: `Ctrl+f` on the last page has to stop at the last row, not fling the
    /// cursor back to the top of the directory. One arrow key's worth of
    /// movement is [`DirState::wrap_cursor`].
    pub fn move_cursor(&mut self, delta: isize) {
        if self.view.is_empty() {
            self.cursor = 0;
            return;
        }
        let last = self.view.len() - 1;
        let next = self.cursor as isize + delta;
        self.cursor = next.clamp(0, last as isize) as usize;
        self.wanted_cursor = None;
        self.remember_cursor();
    }

    /// Move the cursor by `delta` rows, **wrapping** at both ends.
    ///
    /// What `↑`/`↓` do: a listing is a ring, so `↓` on the last row is the
    /// first row and `↑` on the first is the last. A directory is read far more
    /// often than it is paged through, and the alternative — a key that does
    /// nothing at the edge — costs a `g g` or a `G` to say the same thing.
    /// Pages still clamp ([`DirState::move_cursor`]), because a page is a
    /// distance rather than a step.
    pub fn wrap_cursor(&mut self, delta: isize) {
        if self.view.is_empty() {
            self.cursor = 0;
            return;
        }
        let len = self.view.len() as isize;
        // `rem_euclid` rather than `%`: the remainder of a negative number is
        // negative in Rust, and `↑` on the first row is exactly that case.
        self.cursor = (self.cursor as isize + delta).rem_euclid(len) as usize;
        self.remember_cursor();
    }

    /// Put the cursor on a named file if it is visible. Returns whether it was.
    /// This is `g`-goto-into-a-directory landing on the child you came out of.
    pub fn cursor_to_name(&mut self, name: &str) -> bool {
        let Some(position) = self.position_of(name) else {
            return false;
        };
        self.cursor = position;
        self.wanted_cursor = None;
        self.remember_cursor();
        true
    }

    /// Aim the cursor at a name that may not have arrived yet.
    ///
    /// [`DirState::cursor_to_name`] can only work on rows that are already
    /// here, and the callers that know where the cursor belongs — `←` landing
    /// on the child you stepped out of, a tab returning to a directory it
    /// remembers — ask *while the scan is still in flight*, when the listing is
    /// empty and there is nothing to point at. The name is kept and the next
    /// batch that contains it moves the cursor; a scan that finishes without it
    /// gives up (the file was deleted, or `.` is off), and a cursor the user
    /// moves themselves cancels the aim rather than being yanked off it later.
    pub fn aim_cursor(&mut self, name: impl Into<String>) {
        let name = name.into();
        if self.cursor_to_name(&name) {
            return;
        }
        self.wanted_cursor = Some(name);
    }

    /// The view position of a named file, if it is visible.
    pub fn position_of(&self, name: &str) -> Option<usize> {
        self.view
            .iter()
            .position(|i| self.entries.get(*i).is_some_and(|e| e.name == name))
    }

    fn remember_cursor(&mut self) {
        self.cursor_name = self.row(self.cursor).map(|e| e.name.clone());
    }

    // ── Sort, hidden, filter ────────────────────────────────────────────────

    pub fn sort_options(&self) -> SortOptions {
        self.sort
    }

    /// Re-sort. The cursor stays on the same *file*, which is what makes `, m`
    /// on a big directory tolerable — the row you were looking at is still the
    /// row you are looking at, it just moved.
    pub fn set_sort(&mut self, sort: SortOptions) {
        self.sort = sort;
        self.rebuild();
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// The `.` toggle.
    pub fn set_show_hidden(&mut self, show: bool) {
        if self.show_hidden == show {
            return;
        }
        self.show_hidden = show;
        self.rebuild();
    }

    pub fn toggle_hidden(&mut self) {
        self.set_show_hidden(!self.show_hidden);
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The `f` query, applied as you type.
    pub fn set_filter(&mut self, query: impl Into<String>) {
        self.filter = query.into();
        self.rebuild();
    }

    pub fn clear_filter(&mut self) {
        self.set_filter(String::new());
    }

    /// `/`, `?`, `n`, `N`: move the cursor to the next match, wrapping.
    /// Returns whether anything matched.
    pub fn find(&mut self, query: &str, direction: FindDirection) -> bool {
        let Some(position) = find_from(&self.entries, &self.view, self.cursor, query, direction)
        else {
            return false;
        };
        self.cursor = position;
        self.wanted_cursor = None;
        self.remember_cursor();
        true
    }

    // ── Selection ───────────────────────────────────────────────────────────

    pub fn is_selected(&self, name: &str) -> bool {
        self.selected.contains(name)
    }

    /// Selected files, in name order, as paths — the input to every operation
    /// in PLAN §5.
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.selected.iter().map(|n| self.path.join(n)).collect()
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    /// `Space`: toggle the row under the cursor. The caller advances the cursor
    /// afterwards (PLAN §4.1's "toggle select + advance") — the advance is a
    /// keybinding's decision, not the model's.
    pub fn toggle_selected(&mut self, position: usize) {
        let Some(name) = self.row(position).map(|e| e.name.clone()) else {
            return;
        };
        if !self.selected.remove(&name) {
            self.selected.insert(name);
        }
    }

    /// `Ctrl+a`. Only the *visible* rows: selecting rows a filter is hiding
    /// would mean `d` deleting files you cannot see.
    pub fn select_all(&mut self) {
        for position in 0..self.view.len() {
            if let Some(name) = self.row(position).map(|e| e.name.clone()) {
                self.selected.insert(name);
            }
        }
    }

    /// `Ctrl+r`. Again, only over the visible rows.
    pub fn invert_selection(&mut self) {
        for position in 0..self.view.len() {
            let Some(name) = self.row(position).map(|e| e.name.clone()) else {
                continue;
            };
            if !self.selected.remove(&name) {
                self.selected.insert(name);
            }
        }
    }

    /// The last rung of the `Esc` ladder that concerns this model.
    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// Select a run of rows — visual mode (`v`) committing its range.
    pub fn select_range(&mut self, from: usize, to: usize, selected: bool) {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        for position in lo..=hi.min(self.view.len().saturating_sub(1)) {
            let Some(name) = self.row(position).map(|e| e.name.clone()) else {
                continue;
            };
            if selected {
                self.selected.insert(name);
            } else {
                self.selected.remove(&name);
            }
        }
    }

    // ── Scanning ────────────────────────────────────────────────────────────

    /// Ask for a (re)scan and start ignoring the previous one.
    ///
    /// The entries stay on screen until the new scan's first batch replaces
    /// them: a rescan must not blink the pane empty, or every file that lands
    /// during a copy would make the list flash.
    pub fn begin_scan(&mut self, scanner: &Scanner) -> ScanToken {
        let token = scanner.scan(self.path.clone());
        self.token = Some(token);
        self.state = LoadState::Loading;
        self.error = None;
        token
    }

    /// The token this listing is waiting for.
    pub fn token(&self) -> Option<ScanToken> {
        self.token
    }

    /// Take one update. Returns whether it was for this listing — an update
    /// from a stale scan, or for another directory, is dropped and reported as
    /// `false` so a caller can route it elsewhere.
    pub fn apply(&mut self, update: &ScanUpdate) -> bool {
        if Some(update.token()) != self.token || update.dir() != self.path {
            return false;
        }
        match update {
            ScanUpdate::Started { .. } => {
                // The point at which the old listing is dropped: late enough
                // that the pane never blinks, early enough that the first batch
                // is not appended to the previous contents.
                self.entries.clear();
                self.state = LoadState::Loading;
            }
            ScanUpdate::Batch { entries, .. } => {
                self.entries.extend(entries.iter().cloned());
                // Re-sorting on every batch is quadratic in the batch count,
                // which is fine for the directories a person looks at and
                // ruinous for the ones they do not — see `STREAM_SORT_LIMIT`.
                if self.entries.len() <= STREAM_SORT_LIMIT {
                    self.rebuild();
                }
            }
            ScanUpdate::Done { .. } => {
                self.state = LoadState::Loaded;
                self.rebuild();
                // Everything the directory has is here, so a name still being
                // waited for is not coming: stop, rather than pouncing on a
                // file that happens to be created there later.
                self.wanted_cursor = None;
                // A selection can only contain files that are still here. Left
                // alone, a `y` on a file someone else deleted would paste a
                // ghost.
                let present: BTreeSet<String> =
                    self.entries.iter().map(|e| e.name.clone()).collect();
                self.selected.retain(|name| present.contains(name));
            }
            ScanUpdate::Failed { error, .. } => {
                self.entries.clear();
                self.state = LoadState::Failed;
                self.error = Some(error.to_string());
                self.rebuild();
            }
        }
        true
    }

    /// Fill this listing from rows a caller already has, rather than from a
    /// directory read.
    ///
    /// The virtual-listing door. An archive browsed as a directory (PLAN §7.3)
    /// has rows no `readdir` produces — and everything else a listing does is a
    /// pure function of its [`Entry`]s: the sort, the dir-first rule, the `f`
    /// filter, `/` and `n`, the selection, the cursor-keeps-its-name rule, and
    /// the whole row painter downstream of them. All of that should work
    /// unchanged inside an archive, so the entries arrive by hand and the rest
    /// of the type never learns the difference.
    ///
    /// The scan token is cleared, which is what makes this safe next to the
    /// asynchronous path: a late [`ScanUpdate`] for a scan this listing used to
    /// be waiting on is dropped by [`DirState::apply`] rather than overwriting
    /// rows that did not come from a filesystem.
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.state = LoadState::Loaded;
        self.error = None;
        self.token = None;
        self.rebuild();
    }

    /// Begin a listing whose rows will arrive in batches from somewhere other
    /// than the local scanner — PLAN §7.6's remote panes.
    ///
    /// [`DirState::set_entries`] is the whole-listing-at-once door and lands
    /// the pane in [`LoadState::Loaded`]; a remote directory streams, and until
    /// its last batch has arrived the pane is genuinely *loading* — which is
    /// the state the 150 ms "loading…" hint reads, and the one thing a link
    /// with real latency must be honest about. Clearing the token is the same
    /// safety [`DirState::set_entries`] takes: a late [`ScanUpdate`] for a scan
    /// this listing used to be waiting on is dropped rather than overwriting
    /// rows that did not come from a filesystem.
    pub fn begin_external(&mut self) {
        self.entries.clear();
        self.state = LoadState::Loading;
        self.error = None;
        self.token = None;
        self.rebuild();
    }

    /// One batch of an external listing.
    pub fn extend_external(&mut self, entries: Vec<Entry>) {
        self.entries.extend(entries);
        // The same guard [`DirState::apply`] uses: re-sorting per batch is
        // quadratic in the batch count, and a remote directory of a hundred
        // thousand files must not become quadratic because it arrives a
        // hundred rows at a time.
        if self.entries.len() <= STREAM_SORT_LIMIT {
            self.rebuild();
        }
    }

    /// Every row of an external listing has arrived.
    pub fn finish_external(&mut self) {
        self.state = LoadState::Loaded;
        self.rebuild();
        // Same rule as a finished scan: a selection can only contain rows that
        // are still here.
        let present: BTreeSet<String> = self.entries.iter().map(|e| e.name.clone()).collect();
        self.selected.retain(|name| present.contains(name));
    }

    /// An external listing could not be produced. The message is the one the
    /// pane shows in place of rows, so it has to be a sentence.
    pub fn fail_external(&mut self, error: impl Into<String>) {
        self.entries.clear();
        self.state = LoadState::Failed;
        self.error = Some(error.into());
        self.rebuild();
    }

    /// Rewrite the entries in place, and rebuild the view if anything moved.
    ///
    /// The one mutable door onto rows that are already here, opened for PLAN
    /// §7.3's "what's big" mode: a recursive directory size arrives *after* the
    /// scan that produced the row, and it has to land in
    /// [`Entry::len`](crate::fs::Entry::len) itself rather than beside it —
    /// because that is the field the size sort reads, and the whole point of
    /// the mode is that the biggest thing floats to the top while the walk is
    /// still running.
    ///
    /// `revise` returns whether it changed anything; a `false` skips the
    /// rebuild, so a walk that reports the same numbers again costs one pass
    /// over the rows and no re-sort.
    pub fn revise_entries(&mut self, revise: impl FnOnce(&mut [Entry]) -> bool) -> bool {
        if !revise(&mut self.entries) {
            return false;
        }
        self.rebuild();
        true
    }

    /// Load synchronously. For startup's parent pane and for tests; everything
    /// interactive goes through [`DirState::begin_scan`].
    pub fn load_blocking(&mut self) -> Result<(), DfError> {
        match scan_blocking(&self.path) {
            Ok(entries) => {
                self.entries = entries;
                self.state = LoadState::Loaded;
                self.error = None;
                self.rebuild();
                Ok(())
            }
            Err(e) => {
                self.entries.clear();
                self.state = LoadState::Failed;
                self.error = Some(e.to_string());
                self.rebuild();
                Err(e)
            }
        }
    }

    /// Recompute the view from the entries, and put the cursor back on the file
    /// it was on.
    ///
    /// The cursor rule, in order: the same *name* if it is still visible;
    /// otherwise the same *position*, clamped. Name first because that is what
    /// "the cursor did not move" means to a person watching a directory change
    /// under them; position as the fallback because when the file you were on
    /// is deleted, the sensible place to be is where it was.
    fn rebuild(&mut self) {
        let order = sort_order(&self.entries, &self.sort);
        let matched = filter::filter_indices(&self.entries, &order, &self.filter, self.show_hidden);
        self.view = matched.iter().map(|m| m.index).collect();
        self.spans = matched.into_iter().map(|m| m.spans).collect();
        self.generation = self.generation.wrapping_add(1);

        if self.view.is_empty() {
            self.cursor = 0;
            return;
        }
        // An aimed name beats the remembered one: it is the more recent
        // instruction, from a caller that knows where this listing is being
        // opened *to*.
        if let Some(position) = self
            .wanted_cursor
            .as_deref()
            .and_then(|name| self.position_of(name))
        {
            self.wanted_cursor = None;
            self.cursor = position;
            self.remember_cursor();
            return;
        }
        let restored = self
            .cursor_name
            .as_deref()
            .and_then(|name| self.position_of(name));
        self.cursor = match restored {
            Some(position) => position,
            None => self.cursor.min(self.view.len() - 1),
        };
        self.remember_cursor();
    }
}
