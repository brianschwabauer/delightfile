//! What this directory looked like last time (PLAN §2).
//!
//! Grid view is per-directory: `~/Pictures` and the plex mount want thumbnails,
//! `~/src` wants a list, and being asked which one every single time would be
//! the kind of friction the whole program exists to remove. PLAN §2 calls for "a
//! small state db (`~/.local/state/delightfile/`)", and this is it — plus the
//! three other per-directory overrides that belong in the same file for the same
//! reason (a sort you chose *here*, a linemode you chose *here*, hidden files
//! you turned on *here*), plus the tab list, so session restore later has
//! somewhere to read from.
//!
//! It is a preferences file, not a database. Losing it costs a shrug.
//!
//! # The format
//!
//! One record per line, UTF-8, no sections, no nesting:
//!
//! ```text
//! # delightfile state v1
//! /home/brian/Pictures\tview=grid\tlinemode=none\tt=1756598400
//! /home/brian/src\tsort=mtime\tsort_reverse=1\tt=1756598000
//! !tabs\t0=/home/brian\t1=/tmp\tactive=1\tt=1756598400
//! ```
//!
//! - Fields are separated by **tabs**. The first field is the record's key: an
//!   absolute path, or `!tabs` for the one global record. Paths are absolute and
//!   `!` is not a path, so the two can never collide.
//! - Every later field is `key=value`, split at the **first** `=` so a value may
//!   contain one.
//! - Escaping, applied to keys and values alike: `\\` for a backslash, `\t` for
//!   a tab, `\n` and `\r` for the line endings, `\xNN` for any other byte below
//!   0x20 or equal to 0x7f. Everything else — every non-ASCII byte included — is
//!   written through untouched, so an ordinary path with Japanese in it is
//!   readable in the file rather than a wall of hex.
//! - Paths are handled as **bytes**, not `String`. A Unix filename is a bag of
//!   bytes that is usually UTF-8, and a file manager that forgot a directory
//!   because its name was Latin-1 would be broken in exactly the place it is
//!   supposed to be sturdy. The `\xNN` escape is what carries those bytes.
//! - An unparseable line is dropped with a warning and the rest of the file
//!   loads — same rule as the config parser (PLAN §3). Since a save rewrites the
//!   whole file from memory, the next [`StateStore::flush`] quietly repairs it.
//!
//! ## Why not the TOML parser this crate already has
//!
//! Because this file is *written*, constantly — every grid toggle, every sort
//! change, every directory entered — and it is written by a program, read by a
//! program, and only glanced at by a human debugging something. TOML's value is
//! that people author it by hand; there is nothing to author here. Round-tripping
//! through a document model would mean formatting decisions, comment
//! preservation, and quoting rules for exactly the byte sequences that are the
//! hard part above, in exchange for nothing. A line per record appends, diffs and
//! recovers from corruption a line at a time.
//!
//! ## Who owns the timing
//!
//! **The app does.** There is no thread in here, no timer, no `Drop` that
//! writes. df-core stays io-simple: the store holds records in memory, marks
//! itself dirty, and writes only when someone calls [`StateStore::flush`].
//! Debouncing — "500 ms after the last change, and once more on quit" — is the
//! app's business, because the app is the thing with an event loop and the thing
//! that knows when the user has stopped fiddling. A background writer thread
//! here would give this crate a lifetime, a shutdown order and a test that has to
//! sleep, all to save the caller a timer it already has.
//!
//! The write itself is atomic: a temp file beside the target, then `rename`.
//! A crash mid-save leaves either the old file or the new one, never half of
//! either, and never a state file that has been truncated to zero.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{LineMode, SortBy};
use crate::{DfError, Result};

#[cfg(test)]
mod tests;

/// How many directories are remembered.
///
/// Each record is a path and four small fields — call it 150 bytes on the heap
/// and a similar line on disk — so 2,000 of them is ~300 KB in memory and a
/// file that loads in a millisecond. It is also far more directories than
/// anyone deliberately customises: the ones that get a grid view are the media
/// ones, and there are a dozen. The cap exists because the *touch* is automatic
/// — entering a directory can record it — so without a bound the file would
/// grow for the life of the installation.
///
/// Eviction is least-recently-touched, which for this data is exactly right: the
/// directory you have not opened in six months is the one whose grid preference
/// you will not miss.
pub const MAX_STATE_ENTRIES: usize = 2000;

/// The largest state file that will be read, in bytes.
///
/// [`MAX_STATE_ENTRIES`] records at a generous 400 bytes each is 800 KB; 4 MiB
/// is five times that. Past it the file is not this program's — something else
/// wrote there, or it was corrupted into something enormous — and the right move
/// is to warn, start empty and rewrite it, not to spend a second parsing it.
pub const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;

/// The first line of a state file.
pub const HEADER: &str = "# delightfile state v1";

/// The key of the one non-directory record. Not a valid absolute path, which is
/// what keeps the namespace unambiguous without a section syntax.
pub const TABS_KEY: &str = "!tabs";

/// How a directory is drawn (PLAN §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// Miller-column rows.
    #[default]
    List,
    /// Thumbnail tiles.
    Grid,
}

impl View {
    pub fn from_name(name: &str) -> Option<View> {
        match name {
            "list" => Some(View::List),
            "grid" => Some(View::Grid),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            View::List => "list",
            View::Grid => "grid",
        }
    }

    /// The other one. What the toggle key does.
    pub fn toggled(self) -> View {
        match self {
            View::List => View::Grid,
            View::Grid => View::List,
        }
    }
}

/// A sort chosen for one directory.
///
/// Direction rides along with the key because half a sort override is not a
/// setting: choosing "newest first" here and having the direction come from the
/// global config would give you oldest-first the moment that config changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortOverride {
    pub by: SortBy,
    pub reverse: bool,
}

/// Everything remembered about one directory. Every field is optional and
/// `None` means "no override — use the config", which is why this is not just
/// `MgrConfig` with defaults filled in: the store has to be able to tell "you
/// chose list here" from "you have never said".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewState {
    pub view: Option<View>,
    pub sort: Option<SortOverride>,
    pub linemode: Option<LineMode>,
    pub show_hidden: Option<bool>,
}

impl ViewState {
    /// Whether anything is set. An empty state is not stored — see
    /// [`StateStore::set_view`] and friends.
    pub fn is_empty(&self) -> bool {
        self.view.is_none()
            && self.sort.is_none()
            && self.linemode.is_none()
            && self.show_hidden.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    state: ViewState,
    /// Unix seconds, for the LRU. Stored in the file so the ordering survives a
    /// restart — an LRU that resets on launch evicts by load order, which is to
    /// say arbitrarily.
    touched: u64,
}

/// The store: load it at startup, read and write it as the user moves around,
/// [`StateStore::flush`] it when the app's debounce timer says so.
#[derive(Debug)]
pub struct StateStore {
    path: PathBuf,
    dirs: HashMap<PathBuf, Record>,
    tabs: Vec<PathBuf>,
    active_tab: usize,
    tabs_touched: u64,
    dirty: bool,
}

impl StateStore {
    /// Where the state file lives: `$XDG_STATE_HOME/delightfile/state`, falling
    /// back to `~/.local/state/delightfile/state` — which is what the XDG spec
    /// says the variable defaults to, so the fallback is the spec and not a
    /// guess. `None` when neither variable is set, in which case the store works
    /// entirely in memory and nothing is persisted.
    ///
    /// State, not config and not cache: this is data the program generated that
    /// should survive a reboot but that nobody would put in a dotfiles repo.
    /// That is precisely the directory `$XDG_STATE_HOME` was added for.
    pub fn state_path() -> Option<PathBuf> {
        state_path_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
    }

    /// Load from the default location. Never fails: a missing file is a fresh
    /// store, an unreadable one is a warning and a fresh store, and a store with
    /// nowhere to save to still works for the length of the session.
    pub fn load() -> StateStore {
        match StateStore::state_path() {
            Some(path) => StateStore::load_from(path),
            None => {
                log::warn!(
                    "state: no $XDG_STATE_HOME and no $HOME; view settings are session-only"
                );
                StateStore::empty(PathBuf::new())
            }
        }
    }

    /// Load from an explicit path. For tests, and for a `--state` flag if one
    /// ever exists.
    pub fn load_from(path: impl Into<PathBuf>) -> StateStore {
        let path = path.into();
        let mut store = StateStore::empty(path.clone());
        let bytes = match std::fs::metadata(&path) {
            Err(_) => return store, // never written; not an error
            Ok(meta) if meta.len() > MAX_STATE_BYTES => {
                log::warn!(
                    "state: {} is {} bytes, past the {MAX_STATE_BYTES}-byte limit; ignoring it",
                    path.display(),
                    meta.len()
                );
                return store;
            }
            Ok(_) => match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    log::warn!("state: {}: {e}", path.display());
                    return store;
                }
            },
        };
        store.parse(&bytes);
        store
    }

    fn empty(path: PathBuf) -> StateStore {
        StateStore {
            path,
            dirs: HashMap::new(),
            tabs: Vec::new(),
            active_tab: 0,
            tabs_touched: 0,
            dirty: false,
        }
    }

    /// Where this store saves, empty when it has nowhere to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether there are unsaved changes. The app's debounce timer reads this to
    /// skip a write it does not need.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn len(&self) -> usize {
        self.dirs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }

    // ── reading ─────────────────────────────────────────────────────────────

    /// Everything remembered about `dir`, or `None` if it has never been
    /// customised. A read does not count as a touch: opening a directory to look
    /// at it should not be what keeps a preference alive — the preference is
    /// kept alive by *having* one, and refreshed by [`StateStore::touch`] when
    /// the app decides a visit is worth recording.
    pub fn get(&self, dir: &Path) -> Option<ViewState> {
        self.dirs.get(dir).map(|r| r.state)
    }

    /// This directory's view, or `None` to mean "whatever the config says".
    pub fn view(&self, dir: &Path) -> Option<View> {
        self.dirs.get(dir).and_then(|r| r.state.view)
    }

    pub fn sort(&self, dir: &Path) -> Option<SortOverride> {
        self.dirs.get(dir).and_then(|r| r.state.sort)
    }

    pub fn linemode(&self, dir: &Path) -> Option<LineMode> {
        self.dirs.get(dir).and_then(|r| r.state.linemode)
    }

    pub fn show_hidden(&self, dir: &Path) -> Option<bool> {
        self.dirs.get(dir).and_then(|r| r.state.show_hidden)
    }

    /// The directories the tabs were last on, oldest tab first. Empty until
    /// something has called [`StateStore::set_tabs`].
    pub fn tabs(&self) -> &[PathBuf] {
        &self.tabs
    }

    /// Which tab was focused, as an index into [`StateStore::tabs`]. Clamped on
    /// load, so a truncated file cannot hand the app an out-of-range index.
    pub fn active_tab(&self) -> usize {
        self.active_tab
    }

    // ── writing ─────────────────────────────────────────────────────────────

    /// Set or clear the view override. `None` clears it; a record left with no
    /// overrides at all is removed entirely, so turning grid off does not leave
    /// a line behind holding an LRU slot.
    pub fn set_view(&mut self, dir: impl Into<PathBuf>, view: Option<View>) {
        self.update(dir, |state| state.view = view);
    }

    pub fn set_sort(&mut self, dir: impl Into<PathBuf>, sort: Option<SortOverride>) {
        self.update(dir, |state| state.sort = sort);
    }

    pub fn set_linemode(&mut self, dir: impl Into<PathBuf>, linemode: Option<LineMode>) {
        self.update(dir, |state| state.linemode = linemode);
    }

    pub fn set_hidden(&mut self, dir: impl Into<PathBuf>, show_hidden: Option<bool>) {
        self.update(dir, |state| state.show_hidden = show_hidden);
    }

    /// Replace everything remembered about one directory at once.
    pub fn set(&mut self, dir: impl Into<PathBuf>, state: ViewState) {
        self.update(dir, |slot| *slot = state);
    }

    /// Forget a directory. Returns whether there was anything to forget.
    pub fn clear(&mut self, dir: &Path) -> bool {
        let removed = self.dirs.remove(dir).is_some();
        self.dirty |= removed;
        removed
    }

    /// Mark a directory as recently used without changing its settings, so the
    /// LRU keeps it. A no-op for a directory with no record — visiting a
    /// directory is not a reason to remember it, having chosen something there
    /// is.
    pub fn touch(&mut self, dir: &Path) {
        if let Some(record) = self.dirs.get_mut(dir) {
            record.touched = now();
            self.dirty = true;
        }
    }

    /// Record where the tabs are, for session restore.
    pub fn set_tabs(&mut self, tabs: Vec<PathBuf>, active: usize) {
        self.tabs = tabs;
        self.active_tab = active.min(self.tabs.len().saturating_sub(1));
        self.tabs_touched = now();
        self.dirty = true;
    }

    fn update(&mut self, dir: impl Into<PathBuf>, edit: impl FnOnce(&mut ViewState)) {
        let dir = dir.into();
        let mut state = self.dirs.get(&dir).map(|r| r.state).unwrap_or_default();
        edit(&mut state);
        self.dirty = true;
        if state.is_empty() {
            self.dirs.remove(&dir);
            return;
        }
        self.dirs.insert(
            dir,
            Record {
                state,
                touched: now(),
            },
        );
        self.evict();
    }

    /// Drop least-recently-touched records until the cap is met.
    ///
    /// Done on insert rather than on save so the in-memory size is bounded too,
    /// and one at a time rather than in a batch because insertions arrive one at
    /// a time and the loop therefore runs zero or one times in practice.
    fn evict(&mut self) {
        while self.dirs.len() > MAX_STATE_ENTRIES {
            let Some(oldest) = self
                .dirs
                .iter()
                // Path as the tiebreaker, so eviction is deterministic when a
                // burst of changes lands inside the same second.
                .min_by(|a, b| a.1.touched.cmp(&b.1.touched).then(a.0.cmp(b.0)))
                .map(|(path, _)| path.clone())
            else {
                return;
            };
            self.dirs.remove(&oldest);
        }
    }

    // ── saving ──────────────────────────────────────────────────────────────

    /// Write the file, atomically, if anything has changed.
    ///
    /// The app calls this from its own debounce — see the module essay on why
    /// the timer is not in here. A clean store writes nothing and returns `Ok`.
    /// A store with no path (no `$XDG_STATE_HOME`, no `$HOME`) also writes
    /// nothing and returns `Ok`: the session still works, it just does not
    /// outlive itself.
    pub fn flush(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if self.path.as_os_str().is_empty() {
            self.dirty = false;
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| DfError::io(parent, e))?;
        }

        let body = self.render();
        // Beside the target, never in `/tmp`: `rename` is only atomic within one
        // filesystem, and `$XDG_STATE_HOME` on a different mount from `/tmp` is
        // the ordinary case, not the exotic one.
        let temp =
            self.path
                .with_file_name(format!(".state.tmp.{}.{}", std::process::id(), now_nanos()));
        if let Err(e) = std::fs::write(&temp, &body) {
            let _ignored = std::fs::remove_file(&temp);
            return Err(DfError::io(temp, e));
        }
        if let Err(e) = std::fs::rename(&temp, &self.path) {
            // The rename is the commit. If it fails there is a stray temp file,
            // and leaving it would mean a directory that slowly fills with them.
            let _ignored = std::fs::remove_file(&temp);
            return Err(DfError::io(self.path.clone(), e));
        }
        self.dirty = false;
        Ok(())
    }

    /// The file's exact bytes. Public so a test can check the format without
    /// touching a disk, and so a `--dump-state` could exist without a second
    /// serializer.
    pub fn render(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 * (self.dirs.len() + 2));
        out.extend_from_slice(HEADER.as_bytes());
        out.push(b'\n');

        // Sorted, so the file is stable between saves that changed nothing.
        // A state file that reshuffles itself on every write is one that looks
        // modified to every backup tool that watches it.
        let mut keys: Vec<&PathBuf> = self.dirs.keys().collect();
        keys.sort();
        for key in keys {
            let Some(record) = self.dirs.get(key) else {
                continue;
            };
            out.extend_from_slice(&escape(path_bytes(key)));
            let state = &record.state;
            if let Some(view) = state.view {
                push_field(&mut out, "view", view.name().as_bytes());
            }
            if let Some(sort) = state.sort {
                push_field(&mut out, "sort", sort_name(sort.by).as_bytes());
                push_field(&mut out, "sort_reverse", bool_bytes(sort.reverse));
            }
            if let Some(linemode) = state.linemode {
                push_field(&mut out, "linemode", linemode_name(linemode).as_bytes());
            }
            if let Some(hidden) = state.show_hidden {
                push_field(&mut out, "hidden", bool_bytes(hidden));
            }
            push_field(&mut out, "t", record.touched.to_string().as_bytes());
            out.push(b'\n');
        }

        if !self.tabs.is_empty() {
            out.extend_from_slice(TABS_KEY.as_bytes());
            for (i, tab) in self.tabs.iter().enumerate() {
                push_field(&mut out, &i.to_string(), path_bytes(tab));
            }
            push_field(&mut out, "active", self.active_tab.to_string().as_bytes());
            push_field(&mut out, "t", self.tabs_touched.to_string().as_bytes());
            out.push(b'\n');
        }
        out
    }

    fn parse(&mut self, bytes: &[u8]) {
        for (index, line) in bytes.split(|b| *b == b'\n').enumerate() {
            let line = strip_cr(line);
            if line.is_empty() || line[0] == b'#' {
                continue;
            }
            let mut fields = line.split(|b| *b == b'\t');
            let Some(key) = fields.next() else { continue };
            let Some(key) = unescape(key) else {
                log::warn!(
                    "state: {}:{}: unreadable escape in the key; line skipped",
                    self.path.display(),
                    index + 1
                );
                continue;
            };
            if key.as_slice() == TABS_KEY.as_bytes() {
                self.parse_tabs(fields, index + 1);
                continue;
            }
            if !key.starts_with(b"/") {
                log::warn!(
                    "state: {}:{}: key is not an absolute path; line skipped",
                    self.path.display(),
                    index + 1
                );
                continue;
            }

            let mut state = ViewState::default();
            let mut sort_by: Option<SortBy> = None;
            let mut sort_reverse = false;
            let mut touched = 0u64;
            let mut bad = false;
            for field in fields {
                let Some((name, value)) = split_field(field) else {
                    bad = true;
                    break;
                };
                match name.as_slice() {
                    b"view" => state.view = View::from_name(&text(&value)),
                    b"sort" => sort_by = SortBy::from_name(&text(&value)),
                    b"sort_reverse" => sort_reverse = value.as_slice() == b"1".as_slice(),
                    b"linemode" => state.linemode = LineMode::from_name(&text(&value)),
                    b"hidden" => state.show_hidden = Some(value.as_slice() == b"1".as_slice()),
                    b"t" => touched = text(&value).parse().unwrap_or(0),
                    // An unknown key is a newer delightfile's, or a typo in a
                    // hand edit. Neither is worth dropping the line over — the
                    // other settings on it are still good — but it does not
                    // survive the next save, and that is the honest trade for
                    // not carrying a bag of strings per record forever.
                    _ => log::debug!(
                        "state: {}:{}: unknown key {}",
                        self.path.display(),
                        index + 1,
                        text(&name)
                    ),
                }
            }
            if bad {
                log::warn!(
                    "state: {}:{}: malformed field; line skipped",
                    self.path.display(),
                    index + 1
                );
                continue;
            }
            state.sort = sort_by.map(|by| SortOverride {
                by,
                reverse: sort_reverse,
            });
            if state.is_empty() {
                continue;
            }
            self.dirs.insert(path_from(&key), Record { state, touched });
        }
        // A file that was over the cap — an older build with a bigger one, or a
        // hand-merged pair of files — is trimmed on load rather than carried.
        self.evict();
    }

    fn parse_tabs<'a>(&mut self, fields: impl Iterator<Item = &'a [u8]>, line: usize) {
        let mut numbered: Vec<(usize, PathBuf)> = Vec::new();
        let mut active = 0usize;
        for field in fields {
            let Some((name, value)) = split_field(field) else {
                log::warn!(
                    "state: {}:{line}: malformed tab field; skipped",
                    self.path.display()
                );
                continue;
            };
            match name.as_slice() {
                b"active" => active = text(&value).parse().unwrap_or(0),
                b"t" => self.tabs_touched = text(&value).parse().unwrap_or(0),
                _ => match text(&name).parse::<usize>() {
                    Ok(index) => numbered.push((index, path_from(&value))),
                    Err(_) => log::warn!(
                        "state: {}:{line}: unknown tab field {}",
                        self.path.display(),
                        text(&name)
                    ),
                },
            }
        }
        numbered.sort_by_key(|(index, _)| *index);
        self.tabs = numbered.into_iter().map(|(_, path)| path).collect();
        self.active_tab = active.min(self.tabs.len().saturating_sub(1));
    }
}

/// [`StateStore::state_path`]'s rule, with the environment passed in.
///
/// Split out so the rule can be tested without `set_var`: mutating the process
/// environment from a test is a data race against every other test thread
/// reading `$HOME`, and the environment is the *only* thing about this function
/// that a test would want to vary.
pub fn state_path_from(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(dir) = xdg_state_home {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("delightfile").join("state"));
        }
    }
    let home = home?;
    if home.is_empty() {
        return None;
    }
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("delightfile")
            .join("state"),
    )
}

// ── the format's primitives ─────────────────────────────────────────────────

fn push_field(out: &mut Vec<u8>, name: &str, value: &[u8]) {
    out.push(b'\t');
    out.extend_from_slice(&escape(name.as_bytes()));
    out.push(b'=');
    out.extend_from_slice(&escape(value));
}

/// Split `k=v` at the first `=`, unescaping both halves. `None` when there is no
/// `=` or an escape does not decode — the two ways a field can be nonsense.
fn split_field(field: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let at = field.iter().position(|b| *b == b'=')?;
    let name = unescape(&field[..at])?;
    let value = unescape(&field[at + 1..])?;
    Some((name, value))
}

/// Escape the bytes that would otherwise be structure or line noise. See the
/// module essay for the table.
pub fn escape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for byte in bytes {
        match byte {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'=' => out.extend_from_slice(b"\\x3d"),
            0..=0x1f | 0x7f => {
                out.extend_from_slice(format!("\\x{byte:02x}").as_bytes());
            }
            _ => out.push(*byte),
        }
    }
    out
}

/// Reverse [`escape`]. `None` on a truncated or unknown escape, which is what
/// makes a corrupt line detectable rather than silently becoming a path with a
/// backslash in it.
pub fn unescape(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let next = *bytes.get(i + 1)?;
        match next {
            b'\\' => out.push(b'\\'),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b'x' => {
                let hi = hex(*bytes.get(i + 2)?)?;
                let lo = hex(*bytes.get(i + 3)?)?;
                out.push(hi * 16 + lo);
                i += 2;
            }
            _ => return None,
        }
        i += 2;
    }
    Some(out)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn strip_cr(line: &[u8]) -> &[u8] {
    match line.split_last() {
        Some((b'\r', rest)) => rest,
        _ => line,
    }
}

fn path_bytes(path: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes()
}

fn path_from(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))
}

/// Bytes as text for the fields that are ASCII by construction — enum names,
/// numbers, flags. Lossy on purpose: a garbled `view=gr?d` becomes an unknown
/// name and the override is dropped, which is the same outcome as any other
/// unrecognised value.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn bool_bytes(value: bool) -> &'static [u8] {
    if value {
        b"1"
    } else {
        b"0"
    }
}

/// The spelling [`SortBy::from_name`] accepts, so a file this crate writes is a
/// file this crate reads.
pub fn sort_name(sort: SortBy) -> &'static str {
    match sort {
        SortBy::Alphabetical => "alphabetical",
        SortBy::Natural => "natural",
        SortBy::Extension => "extension",
        SortBy::Size => "size",
        SortBy::Mtime => "mtime",
        SortBy::Btime => "btime",
        SortBy::Random => "random",
        SortBy::None => "none",
    }
}

/// The spelling [`LineMode::from_name`] accepts.
pub fn linemode_name(mode: LineMode) -> &'static str {
    match mode {
        LineMode::Size => "size",
        LineMode::Permissions => "permissions",
        LineMode::Btime => "btime",
        LineMode::Mtime => "mtime",
        LineMode::Owner => "owner",
        LineMode::None => "none",
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
