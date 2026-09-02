//! Remembering what a walk cost, so re-entering a directory is instant.
//!
//! A drill-down is a place you leave and come back to — into `node_modules`,
//! back out, into `target`, back out — and re-walking 80,000 files on the way
//! back is the difference between a mode that feels like ncdu and one that feels
//! broken. So the totals a walk produces are kept, keyed by directory.
//!
//! ## Time is the key, not `mtime`
//!
//! The first version of this keyed freshness on the directory's own `mtime`
//! alone: a different `mtime`, a miss, a re-walk. That is too strict in the
//! direction that costs the most. Saving one file into `~/Downloads` bumps that
//! directory's `mtime`, and the subtree totals under it move by a few kilobytes
//! out of gigabytes — so the whole tree was walked again to redraw the same
//! numbers. Leaving a folder and coming straight back did it every time.
//!
//! It is also too *lax* in the other direction, and always was: a directory's
//! `mtime` says nothing about its grandchildren, so `cargo build` can add 2 GB
//! three levels down and an `mtime` check calls the record fresh. There is no
//! cheap test that catches that; the only honest one is the walk itself, which
//! is the thing being avoided.
//!
//! So freshness is a **clock**. A record walked within [`DuCache::ttl`] is
//! served as it stands, and the two cheap facts — the directory's `mtime` and
//! how many names `read_dir` returns for it — decide only whether a *background*
//! re-walk is owed, not whether the numbers may be shown. Past the TTL the
//! numbers are still shown, and a re-walk is always owed. That gives the three
//! answers the UI actually wants:
//!
//! - **inside the TTL and unchanged** — draw it, walk nothing;
//! - **inside the TTL but changed** — draw it wearing its `~`, re-walk behind it;
//! - **past the TTL** — the same, because a number that is ten minutes old still
//!   beats an em dash for the two seconds a walk takes.
//!
//! A hit remains *a fast answer, not a true one*, and [`DuCache::forget`] is
//! still what "refresh this" is implemented with — but the cost of being
//! approximately right is now paid once every TTL instead of on every keypress.
//!
//! ## Every directory the walk visited, not just the one asked about
//!
//! A walk of `~/Work/delightfile` counts `crates`, `target` and everything
//! under them on the way, so a record is kept for each of them. That is what
//! makes going **up** free as well as coming back in: the listing of `~/Work`
//! is seeded from the records its children already have
//! ([`DuCache::remembered_children`]), and the walk of `~/Work` is handed the
//! same set ([`DuCache::reusable_under`]) so it folds those subtrees in whole
//! instead of counting them a second time. A directory you have just come out
//! of is the one the parent would otherwise spend all its time in.
//!
//! Reuse is an approximation on top of an approximation, and it is opted into
//! rather than assumed — see [`super::walk::DuOptions::reuse_cache`] for what it
//! costs and who declines it.
//!
//! ## Bounded three ways
//!
//! Records, children per record, and children in total — because one huge
//! directory and ten thousand small ones are different ways to eat the same
//! amount of memory, and a cache with only one of the three caps has a shape it
//! is defenceless against. Eviction is least-recently-touched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::walk::DuTotals;

/// How many directories keep a record.
///
/// A drill-down session touches tens of directories; a long day of them, a few
/// hundred. 4,096 is past any of that and, at the per-record cap below, is the
/// outer bound that matters least — [`MAX_CACHE_CHILDREN`] usually bites first.
pub const DU_CACHE_DIRS: usize = 4096;

/// How many children one record keeps.
///
/// The children are what the heavy-hitter list and the size column read, and
/// they are stored **sorted by size, largest first, then truncated** — so a
/// directory with 200,000 subdirectories keeps the 4,096 that could ever appear
/// near the top of a "what's big" list and drops the tail that is, by
/// construction, not big. That makes the cap free for the feature it exists for,
/// and only visible to a caller trying to reconstruct an exact listing from the
/// cache, which is not what the cache is for.
pub const MAX_CACHED_CHILDREN: usize = 4096;

/// How many child entries all records may hold between them.
///
/// A name plus four `u64`s is on the order of 80 bytes, so 100,000 of them is
/// ~8 MB — the point past which a cache for a file manager has stopped being a
/// cache. Records are evicted oldest-touched-first until the total fits.
pub const MAX_CACHE_CHILDREN: usize = 100_000;

/// How long a walk's answer is served without a re-walk.
///
/// Ten minutes is the span of a browsing session's worth of "in here, out
/// again, back in here": long enough that the round trip through a parent
/// directory costs nothing, short enough that a build finishing while you are
/// looking elsewhere is picked up the next time you glance at the folder. It is
/// the default for `[mgr] folder_size_ttl`, which is there for the two people
/// who want either extreme.
pub const DEFAULT_FOLDER_SIZE_TTL: Duration = Duration::from_secs(600);

/// How far the cheap re-check counts names before it stops caring.
///
/// The check is one `read_dir`, and a Maildir is half a million names. Past the
/// cap both the stamp and the re-check saturate to the same number, so a
/// directory that large is validated on its `mtime` alone — which is what it
/// would have had before this cap existed, at a bounded cost.
pub const MAX_STAMP_ENTRIES: u64 = 100_000;

/// The cheap facts about a directory that a revisit re-reads.
///
/// One `stat` and one `read_dir` — microseconds — against a walk that is
/// seconds. Neither can see a change three levels down (see the module essay),
/// and together they are still the best answer available for the price: a file
/// added, removed or renamed at the top level moves both, and a file *rewritten
/// in place* moves only the `mtime`, which is the case worth not re-walking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DirStamp {
    /// The directory's own `mtime`. `None` if the platform would not say.
    pub mtime: Option<SystemTime>,
    /// Names `read_dir` returned, capped at [`MAX_STAMP_ENTRIES`]. `None` if
    /// the directory could not be read.
    pub entries: Option<u64>,
}

impl DirStamp {
    /// Whether `self` and `other` describe the same directory contents, as far
    /// as two cheap facts can tell. A missing half on either side is a `no`:
    /// a stamp that could not be taken cannot agree with anything.
    pub fn agrees_with(&self, other: &DirStamp) -> bool {
        self.mtime.is_some()
            && self.mtime == other.mtime
            && self.entries.is_some()
            && self.entries == other.entries
    }
}

/// A record, and whether the directory still looks the way it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remembered {
    /// The numbers, always. Even a stale record is worth drawing.
    pub record: DuRecord,
    /// `true` when the record is inside the TTL **and** the cheap re-check
    /// agrees with it: nothing needs walking. `false` means draw these numbers
    /// wearing a `~` and start a walk behind them.
    pub fresh: bool,
}

/// One immediate child of a directory, as the cache remembers it.
///
/// What makes going *up* instant. A walk of `~/Work/delightfile` leaves a record
/// for every directory it visited, so the listing of `~/Work` can be seeded from
/// those records — the subtree total for `delightfile` is a number that was
/// computed a moment ago, not one this listing has to earn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildTotal {
    pub name: String,
    pub totals: DuTotals,
    /// Whether the record it came from is inside the TTL.
    pub fresh: bool,
}

/// One directory's remembered totals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuRecord {
    /// What the directory looked like at the moment it was walked. Compared
    /// with a fresh stamp on revisit to decide whether a re-walk is owed — not
    /// whether the numbers may be shown.
    pub stamp: DirStamp,
    /// When the walk that produced this finished. The clock freshness is
    /// actually keyed on; see the module essay.
    pub walked_at: Instant,
    /// The subtree's totals.
    pub totals: DuTotals,
    /// Immediate children, largest first, capped at [`MAX_CACHED_CHILDREN`].
    /// Empty when the walk that produced this record had a
    /// `depth_of_interest` too shallow to see them.
    pub children: Vec<(String, DuTotals)>,
    /// Whether `children` is every child. `false` when the walk saw more than
    /// it was willing to track; see [`DuCache::insert`].
    pub children_complete: bool,
    /// Whether these numbers were **counted** or partly **remembered**.
    ///
    /// A walk with [`super::walk::DuOptions::reuse_cache`] folds in whole
    /// subtrees from records it did not verify below their top directory, so
    /// what it produces is an estimate: a file written three levels down is
    /// invisible to the freshness check, and a file hardlinked between a reused
    /// subtree and a walked one is counted twice.
    ///
    /// That is a fine answer for the size column and a **bad** one to store
    /// where an exact walk reads from, which is what used to happen: the
    /// approximation went into the same cache the next walk reused, so each
    /// reuse compounded the last and "what's big" could be handed an estimate
    /// of an estimate under a mode whose whole promise is a real number. So the
    /// flag travels with the record, and the two callers that must not have one
    /// — [`DuCache::reusable_under`] and [`DuCache::heavy_hitters`] — refuse it.
    pub approximate: bool,
}

/// One row of the "what's big" list.
#[derive(Debug, Clone, PartialEq)]
pub struct HeavyHitter {
    /// The child's name, as it appears in the directory.
    pub name: String,
    /// Its full path, so a drill-down can walk into it without rebuilding one.
    pub path: PathBuf,
    /// Disk usage in bytes — the number the bar is drawn from.
    pub bytes: u64,
    /// Apparent size in bytes, for the row that wants to say "8 GB sparse".
    pub apparent_bytes: u64,
    /// This child's share of the parent's total, `0.0..=1.0`. Zero when the
    /// parent totals zero, rather than `NaN`: a bar of width `NaN` is a panic
    /// waiting in a layout function.
    pub fraction: f64,
}

/// The bounded store.
#[derive(Debug)]
pub struct DuCache {
    records: HashMap<PathBuf, (DuRecord, u64)>,
    children_total: usize,
    clock: u64,
    ttl: Duration,
}

impl Default for DuCache {
    fn default() -> DuCache {
        DuCache {
            records: HashMap::new(),
            children_total: 0,
            clock: 0,
            ttl: DEFAULT_FOLDER_SIZE_TTL,
        }
    }
}

impl DuCache {
    pub fn new() -> DuCache {
        DuCache::default()
    }

    /// A cache with a lifetime other than [`DEFAULT_FOLDER_SIZE_TTL`].
    pub fn with_ttl(ttl: Duration) -> DuCache {
        DuCache {
            ttl,
            ..DuCache::default()
        }
    }

    /// How long a record is served without a re-walk. See the module essay.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Set the lifetime. Existing records keep their `walked_at`, so shortening
    /// it retires them immediately rather than at some future moment.
    pub fn set_ttl(&mut self, ttl: Duration) {
        self.ttl = ttl;
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Child entries held across every record, for the total cap.
    pub fn children_len(&self) -> usize {
        self.children_total
    }

    /// Store a directory's totals.
    ///
    /// `children` is taken by value and re-sorted here rather than at the call
    /// site, so every record in the cache has the same ordering guarantee
    /// whoever built it. `children_complete` is the caller saying whether it
    /// tracked every child or gave up counting — see
    /// [`super::scanner::MAX_TRACKED_DIRS`].
    ///
    /// `walked_at` is passed in rather than read here because one walk produces
    /// thousands of records and they all finished at the same moment; letting
    /// each one stamp itself would spread one answer across a range of times
    /// for no reason.
    pub fn insert(
        &mut self,
        dir: impl Into<PathBuf>,
        stamp: DirStamp,
        walked_at: Instant,
        totals: DuTotals,
        children: Vec<(String, DuTotals)>,
        children_complete: bool,
    ) {
        self.store(
            dir,
            stamp,
            walked_at,
            totals,
            children,
            children_complete,
            false,
        );
    }

    /// [`DuCache::insert`] for a walk that took subtrees out of the cache
    /// rather than counting them. See [`DuRecord::approximate`] for what the
    /// difference costs and who refuses the result.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_approximate(
        &mut self,
        dir: impl Into<PathBuf>,
        stamp: DirStamp,
        walked_at: Instant,
        totals: DuTotals,
        children: Vec<(String, DuTotals)>,
        children_complete: bool,
    ) {
        self.store(
            dir,
            stamp,
            walked_at,
            totals,
            children,
            children_complete,
            true,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn store(
        &mut self,
        dir: impl Into<PathBuf>,
        stamp: DirStamp,
        walked_at: Instant,
        totals: DuTotals,
        mut children: Vec<(String, DuTotals)>,
        children_complete: bool,
        approximate: bool,
    ) {
        let dir = dir.into();
        children.sort_by(|a, b| b.1.total_bytes.cmp(&a.1.total_bytes).then(a.0.cmp(&b.0)));
        let complete = children_complete && children.len() <= MAX_CACHED_CHILDREN;
        children.truncate(MAX_CACHED_CHILDREN);

        self.clock += 1;
        let touched = self.clock;
        if let Some((old, _)) = self.records.remove(&dir) {
            self.children_total = self.children_total.saturating_sub(old.children.len());
        }
        self.children_total += children.len();
        self.records.insert(
            dir,
            (
                DuRecord {
                    stamp,
                    walked_at,
                    totals,
                    children,
                    children_complete: complete,
                    approximate,
                },
                touched,
            ),
        );
        self.evict();
    }

    /// The record for `dir` if it was walked within the TTL. No syscalls: this
    /// is the question "is there a recent answer", not "is it still true".
    /// Touches the record, so lookups keep a working set alive.
    pub fn fresh(&mut self, dir: &Path, now: Instant) -> Option<&DuRecord> {
        let ttl = self.ttl;
        self.clock += 1;
        let touched = self.clock;
        let (record, stamp) = self.records.get_mut(dir)?;
        if now.saturating_duration_since(record.walked_at) > ttl {
            return None;
        }
        *stamp = touched;
        Some(record)
    }

    /// What is remembered about `dir`, and whether it still needs walking.
    ///
    /// Always returns the record when there is one — a stale number is worth
    /// drawing, and so is an approximate one: this is the size column's
    /// question, and the column's job is to say roughly how big a folder is
    /// (the record says which it got, in [`DuRecord::approximate`]). It pays
    /// for one `stat` and one `read_dir` to fill in `fresh`.
    /// A record past the TTL is never fresh and does not pay for the check.
    pub fn remembered(&mut self, dir: &Path, now: Instant) -> Option<Remembered> {
        let record = self.fresh(dir, now).cloned();
        let Some(record) = record else {
            // Past the TTL, or never walked. Only the first has anything to
            // hand back, and it hands it back as stale.
            let record = self.get_stale(dir)?.clone();
            return Some(Remembered {
                record,
                fresh: false,
            });
        };
        let fresh = record.stamp.agrees_with(&current_stamp(dir));
        Some(Remembered { record, fresh })
    }

    /// The record as stored, without any freshness check at all. For a caller
    /// that has decided a remembered number is better than no number — the size
    /// column on a directory that is being rewritten, say, where the
    /// alternative is a blank that flickers.
    pub fn get_stale(&self, dir: &Path) -> Option<&DuRecord> {
        self.records.get(dir).map(|(record, _)| record)
    }

    /// Every immediate child of `dir` the cache can name a size for.
    ///
    /// Two sources, merged: the children carried by `dir`'s own record, and the
    /// records stored for the child directories themselves. **The later walk
    /// wins**, which is what makes going up instant — walking into
    /// `~/Work/delightfile` leaves a record for it that is newer than whatever
    /// the last walk of `~/Work` believed, so stepping back out draws the number
    /// that was just computed rather than the one from before.
    ///
    /// No syscalls, and no TTL filter: a child past the TTL comes back with
    /// `fresh: false` so the caller can draw it and re-walk behind it.
    pub fn remembered_children(&self, dir: &Path, now: Instant) -> Vec<ChildTotal> {
        let mut merged: HashMap<&str, (Instant, DuTotals)> = HashMap::new();
        if let Some((record, _)) = self.records.get(dir) {
            for (name, totals) in &record.children {
                merged.insert(name.as_str(), (record.walked_at, *totals));
            }
        }
        for (path, (record, _)) in &self.records {
            if path.parent() != Some(dir) {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            match merged.get(name) {
                Some((at, _)) if *at >= record.walked_at => {}
                _ => {
                    merged.insert(name, (record.walked_at, record.totals));
                }
            }
        }
        let mut out: Vec<ChildTotal> = merged
            .into_iter()
            .map(|(name, (at, totals))| ChildTotal {
                name: name.to_string(),
                totals,
                fresh: now.saturating_duration_since(at) <= self.ttl,
            })
            .collect();
        out.sort_by(|a, b| {
            b.totals
                .total_bytes
                .cmp(&a.totals.total_bytes)
                .then(a.name.cmp(&b.name))
        });
        out
    }

    /// Every record strictly below `root` that is inside the TTL, as a map a
    /// walk can consult without touching this lock again.
    ///
    /// The walk asks about every directory it meets, once, on a worker thread —
    /// so it gets one snapshot rather than a mutex acquisition per directory
    /// contending with the UI thread. `root` itself is left out: a walk of a
    /// directory is not allowed to answer itself from the cache.
    pub fn reusable_under(
        &self,
        root: &Path,
        now: Instant,
    ) -> HashMap<PathBuf, (DirStamp, DuTotals)> {
        self.records
            .iter()
            .filter(|(path, _)| path.as_path() != root && path.starts_with(root))
            .filter(|(_, (record, _))| now.saturating_duration_since(record.walked_at) <= self.ttl)
            // **An estimate is not something to build the next estimate on.**
            // A record produced by a reusing walk was itself part-remembered,
            // and reusing it would compound the error every time somebody
            // walked up a level — the drift growing with each round trip and
            // nothing ever recounting it. Walking such a subtree costs the
            // syscalls the reuse was meant to save; paying them once is what
            // makes the number come back true.
            .filter(|(_, (record, _))| !record.approximate)
            .map(|(path, (record, _))| (path.clone(), (record.stamp, record.totals)))
            .collect()
    }

    /// Drop one directory's record. What "refresh this" is implemented with.
    pub fn forget(&mut self, dir: &Path) -> bool {
        match self.records.remove(dir) {
            Some((record, _)) => {
                self.children_total = self.children_total.saturating_sub(record.children.len());
                true
            }
            None => false,
        }
    }

    pub fn clear(&mut self) {
        self.records.clear();
        self.children_total = 0;
    }

    /// The "what's big" list for `dir`: children by disk usage, largest first,
    /// each with its share of the parent.
    ///
    /// Empty when nothing is cached for `dir`, when the record has aged out, or
    /// when the walk that filled it ran with `depth_of_interest == 0` and so
    /// never looked at the children. All three cases mean the same thing to the
    /// caller — ask for a walk — which is why they are not distinguished.
    pub fn heavy_hitters(&mut self, dir: &Path, now: Instant) -> Vec<HeavyHitter> {
        let Some(record) = self.fresh(dir, now) else {
            return Vec::new();
        };
        // A fourth case, and it means the same thing to the caller as the other
        // three: **a number somebody asked for out loud has to be the real
        // one** (see [`super::walk::DuOptions::reuse_cache`]). An approximate
        // record is refused here so the mode asks for its own exact walk rather
        // than drawing bars from an estimate the size column left behind.
        if record.approximate {
            return Vec::new();
        }
        let total = record.totals.total_bytes;
        record
            .children
            .iter()
            .map(|(name, totals)| HeavyHitter {
                name: name.clone(),
                path: dir.join(name),
                bytes: totals.total_bytes,
                apparent_bytes: totals.apparent_bytes,
                fraction: if total == 0 {
                    0.0
                } else {
                    totals.total_bytes as f64 / total as f64
                },
            })
            .collect()
    }

    fn evict(&mut self) {
        while self.records.len() > DU_CACHE_DIRS || self.children_total > MAX_CACHE_CHILDREN {
            let Some(oldest) = self
                .records
                .iter()
                .min_by_key(|(_, (_, touched))| *touched)
                .map(|(path, _)| path.clone())
            else {
                return;
            };
            if !self.forget(&oldest) {
                return;
            }
        }
    }
}

/// The cheap facts about `dir`, read now. See [`DirStamp`].
pub fn current_stamp(dir: &Path) -> DirStamp {
    DirStamp {
        mtime: current_mtime(dir),
        entries: count_names(dir),
    }
}

/// How many names `read_dir` returns for `dir`, capped at
/// [`MAX_STAMP_ENTRIES`]. `None` when the directory cannot be read.
///
/// Names, not entries the walk would count: nothing here calls `stat`, because
/// a check that costs a syscall per name is not a cheap check. Symlinks and
/// entries on another filesystem are counted, and the stamp taken during the
/// walk counts them too — the two only ever have to agree with each other.
pub fn count_names(dir: &Path) -> Option<u64> {
    let reader = std::fs::read_dir(dir).ok()?;
    let mut names = 0u64;
    for _ in reader {
        names += 1;
        if names >= MAX_STAMP_ENTRIES {
            break;
        }
    }
    Some(names)
}

/// A directory's modification time, or `None` if it is gone or refuses to say.
pub fn current_mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).ok()?.modified().ok()
}
