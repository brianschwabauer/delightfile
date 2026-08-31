//! Remembering what a walk cost, so re-entering a directory is instant.
//!
//! A drill-down is a place you leave and come back to — into `node_modules`,
//! back out, into `target`, back out — and re-walking 80,000 files on the way
//! back is the difference between a mode that feels like ncdu and one that feels
//! broken. So the totals a walk produces are kept, keyed by directory.
//!
//! ## The mtime key is advisory, and this is the honest part
//!
//! Each record remembers the directory's `mtime` at the moment it was walked,
//! and a lookup with a different `mtime` misses. That catches the common
//! staleness — a file added or removed *directly in that directory* bumps its
//! `mtime`, so the record for it is correctly thrown away.
//!
//! **It does not catch deep changes.** A directory's `mtime` says nothing about
//! its grandchildren: `cargo build` can add 2 GB three levels down and the
//! record for the root still looks fresh. There is no cheap fix — the only
//! honest check is the walk itself, which is the thing being avoided. So the
//! rule this cache is built on is: *a hit is a fast answer, not a true one*. The
//! UI must show a cached figure as remembered rather than live, refreshing is
//! always one keypress away, and [`DuCache::forget`] exists so that keypress
//! costs nothing to implement. Pretending otherwise would put a wrong number on
//! screen with no way for the user to know it was wrong, which is worse than
//! being slow.
//!
//! ## Bounded three ways
//!
//! Records, children per record, and children in total — because one huge
//! directory and ten thousand small ones are different ways to eat the same
//! amount of memory, and a cache with only one of the three caps has a shape it
//! is defenceless against. Eviction is least-recently-touched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

/// One directory's remembered totals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuRecord {
    /// The directory's own `mtime` when it was walked. `None` if the platform
    /// would not give one, in which case every lookup misses — a record that
    /// cannot be invalidated is a record that must not be trusted.
    pub mtime: Option<SystemTime>,
    /// The subtree's totals.
    pub totals: DuTotals,
    /// Immediate children, largest first, capped at [`MAX_CACHED_CHILDREN`].
    /// Empty when the walk that produced this record had a
    /// `depth_of_interest` too shallow to see them.
    pub children: Vec<(String, DuTotals)>,
    /// Whether `children` is every child. `false` when the walk saw more than
    /// it was willing to track; see [`DuCache::insert`].
    pub children_complete: bool,
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
#[derive(Debug, Default)]
pub struct DuCache {
    records: HashMap<PathBuf, (DuRecord, u64)>,
    children_total: usize,
    clock: u64,
}

impl DuCache {
    pub fn new() -> DuCache {
        DuCache::default()
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
    pub fn insert(
        &mut self,
        dir: impl Into<PathBuf>,
        mtime: Option<SystemTime>,
        totals: DuTotals,
        mut children: Vec<(String, DuTotals)>,
        children_complete: bool,
    ) {
        let dir = dir.into();
        children.sort_by(|a, b| b.1.total_bytes.cmp(&a.1.total_bytes).then(a.0.cmp(&b.0)));
        let complete = children_complete && children.len() <= MAX_CACHED_CHILDREN;
        children.truncate(MAX_CACHED_CHILDREN);

        self.clock += 1;
        let stamp = self.clock;
        if let Some((old, _)) = self.records.remove(&dir) {
            self.children_total = self.children_total.saturating_sub(old.children.len());
        }
        self.children_total += children.len();
        self.records.insert(
            dir,
            (
                DuRecord {
                    mtime,
                    totals,
                    children,
                    children_complete: complete,
                },
                stamp,
            ),
        );
        self.evict();
    }

    /// The record for `dir`, if it is there **and** the directory's `mtime` is
    /// unchanged since it was walked. Touches the record, so lookups keep a
    /// working set alive.
    ///
    /// Read the module essay before trusting the answer: an unchanged `mtime`
    /// does not mean an unchanged subtree.
    pub fn get(&mut self, dir: &Path) -> Option<&DuRecord> {
        let live = current_mtime(dir)?;
        self.clock += 1;
        let stamp = self.clock;
        let (record, touched) = self.records.get_mut(dir)?;
        if record.mtime != Some(live) {
            return None;
        }
        *touched = stamp;
        Some(record)
    }

    /// The record as stored, without the `mtime` check. For a caller that has
    /// decided a remembered number is better than no number — the size column
    /// on a directory that is being rewritten, say, where the alternative is a
    /// blank that flickers.
    pub fn get_stale(&self, dir: &Path) -> Option<&DuRecord> {
        self.records.get(dir).map(|(record, _)| record)
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
    /// Empty when nothing is cached for `dir`, when the record is stale, or when
    /// the walk that filled it ran with `depth_of_interest == 0` and so never
    /// looked at the children. All three cases mean the same thing to the
    /// caller — ask for a walk — which is why they are not distinguished.
    pub fn heavy_hitters(&mut self, dir: &Path) -> Vec<HeavyHitter> {
        let Some(record) = self.get(dir) else {
            return Vec::new();
        };
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

/// A directory's modification time, or `None` if it is gone or refuses to say.
pub fn current_mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).ok()?.modified().ok()
}
