//! The recursive walk itself: one directory tree, counted twice.
//!
//! This is the part that costs syscalls, so it is a plain function with no
//! threads in it — the pool in [`super::scanner`] is what makes it background
//! work, and a test can call [`walk`] directly and watch every batch it emits
//! without spawning anything.
//!
//! ## Two numbers, because there are two questions
//!
//! "How big is this directory" has two honest answers and they can differ by
//! orders of magnitude. **Apparent size** is the sum of `st_size` — what the
//! files claim to be, what a copy of them would have to carry. **Block usage**
//! is the sum of `st_blocks * 512` — what the filesystem actually spent, which
//! is the number that tells you why the disk is full. A sparse 8 GB VM image
//! that has only ever been written to in three places reports 8 GB apparent and
//! maybe 40 MB of blocks; a directory of 10,000 one-byte files reports 10 KB
//! apparent and 40 MB of blocks. `du` shows blocks, `ls` shows apparent, and
//! "what's big" mode (PLAN §7.3) is the ncdu replacement, so blocks is the
//! primary and apparent rides along. Both are carried everywhere rather than
//! picked here, because the caller — a size sort, a bar chart, a treemap — is
//! the one that knows which question it is asking.
//!
//! `st_blocks` is always in 512-byte units regardless of the filesystem's block
//! size; that is POSIX, not a Linux detail, and it is why the multiplier below
//! is a named constant rather than something read from `statvfs`.
//!
//! ## What is not counted, and why
//!
//! - **Symlinks**, always skipped — neither followed nor counted. Following
//!   them would double-count (the target is usually inside the same tree) or
//!   escape it entirely (a link to `/` turns a drill-down of `~/notes` into a
//!   walk of the whole machine), and a cycle of two links pointing at each
//!   other's parents would never terminate. Counting the link's own handful of
//!   bytes without following it is what GNU `du` does; it is also noise at the
//!   scale this mode operates on, and leaving links out of `files` entirely
//!   makes "what did you count" a sentence with no footnote.
//! - **Other filesystems**, unless [`DuOptions::cross_filesystems`] says
//!   otherwise. Every entry's `st_dev` is compared with the root's, so a walk of
//!   `/` does not wander into `/proc`, a 4 TB backup drive under `/mnt`, or an
//!   NFS mount that will block for thirty seconds per `stat`. This is `du -x`,
//!   and it is the default for the same reason it is the sane default there.
//! - **Hardlinked content, twice.** A file with `st_nlink > 1` is recorded by
//!   `(st_dev, st_ino)` and its bytes counted only the first time it is seen.
//!   Files with a single link — nearly all of them — never enter the set at
//!   all, which is what keeps it small on a real tree.
//!
//! ## The stack is explicit
//!
//! Recursion would be the shorter code and the wrong shape: the walk has to be
//! interruptible between entries, it has to be able to report the running total
//! of every directory currently open, and it must not depend on the thread's
//! stack surviving a `node_modules` tree. So the frames are a `Vec`, each
//! holding its own `ReadDir` and its own running totals, and a frame's totals
//! are folded into its parent's the moment it pops.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use std::collections::HashSet;

use crate::{DfError, Result};

/// The unit `st_blocks` is counted in, in bytes.
///
/// POSIX fixes this at 512 for every filesystem — it is not the allocation
/// block size and must not be read from `statvfs`. Named because a bare `* 512`
/// next to a byte count looks like a bug worth "fixing".
pub const BLOCK_UNIT: u64 = 512;

/// How deep the walk will go before it refuses to go further.
///
/// Directory cycles cannot happen here — symlinks are skipped and hardlinked
/// directories do not exist on Linux — but a bind mount of a parent into its own
/// child is on the same device and would recurse until `PATH_MAX` stopped it,
/// several thousand `read_dir`s later. 256 is deeper than any real tree (a
/// pathological `node_modules` bottoms out around 30) and shallow enough that
/// hitting the cap costs a log line rather than a minute.
pub const MAX_DEPTH: usize = 256;

/// How many `(dev, inode)` pairs the hardlink set will hold.
///
/// Only files with `st_nlink > 1` are recorded, so on an ordinary tree the set
/// stays empty; the trees that fill it are backup snapshots and package stores,
/// where hardlinking is the entire storage strategy. At 16 bytes per pair plus
/// hash-table overhead, one million entries is roughly 24 MB — a lot, but less
/// than the walk is worth on the exact trees that reach it.
///
/// **Past the cap, deduplication stops and hardlinked bytes are counted once
/// per name.** Totals then read high, the same way `du` without `-l` reads low.
/// The alternative — evicting from the set — would be worse: it would drop
/// dedup silently *and* non-deterministically, so the same tree would total
/// differently on two runs. Overcounting past a documented cliff is at least a
/// number you can reason about.
pub const MAX_HARDLINK_ENTRIES: usize = 1_000_000;

/// Entries between cancellation checks.
///
/// The check is a closure call that reaches a mutex, and this loop runs once per
/// directory entry — millions of times on a home directory. Checking every 256
/// entries bounds the reaction time to roughly a millisecond of `stat` calls
/// (much less warm) while making the check's cost unmeasurable. Directory
/// boundaries are always checked regardless, which is where a walk that has gone
/// somewhere expensive is most likely to be abandoned.
pub const CANCEL_CHECK_ENTRIES: usize = 256;

/// Updates per batch.
///
/// Every batch is a channel send, a wake-up and a repaint. 64 finished
/// directories is a screenful of drill-down rows, so batching to it keeps a walk
/// of a tree with 100k subdirectories to ~1,500 wake-ups instead of 100,000,
/// and still fills the first screen in the first batch.
pub const DU_BATCH: usize = 64;

/// How often the still-running directories report their growing totals.
///
/// This is the number that makes a walk of a huge tree *feel* alive: the root's
/// figure climbing rather than a spinner. 100 ms is four frames at 40 fps —
/// fast enough that the number is visibly counting, slow enough that the
/// repaints it causes are a rounding error against the walk itself. Faster
/// would animate nothing extra, because a digit changing more than ten times a
/// second is unreadable anyway.
pub const UPDATE_INTERVAL: Duration = Duration::from_millis(100);

/// Recursive totals for one directory's whole subtree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DuTotals {
    /// Disk usage in bytes: the sum of `st_blocks * 512`, hardlinks counted
    /// once. The "what's big" number.
    pub total_bytes: u64,
    /// The sum of `st_size`, hardlinks counted once. What a copy would weigh.
    pub apparent_bytes: u64,
    /// Non-directory names seen. **Names**, not distinct inodes: a file with
    /// three hardlinks inside the tree contributes 3 here and its bytes once.
    pub files: u64,
    /// Directories, **including the one these totals describe**. A leaf
    /// directory with two files reports `dirs: 1, files: 2`.
    pub dirs: u64,
}

impl DuTotals {
    /// Fold a subtree's totals into this one. Saturating, because a `u64` of
    /// bytes cannot realistically overflow but a corrupt `st_size` can lie.
    pub fn add(&mut self, other: &DuTotals) {
        self.total_bytes = self.total_bytes.saturating_add(other.total_bytes);
        self.apparent_bytes = self.apparent_bytes.saturating_add(other.apparent_bytes);
        self.files = self.files.saturating_add(other.files);
        self.dirs = self.dirs.saturating_add(other.dirs);
    }
}

/// One directory's state in a stream of updates.
///
/// Emitted repeatedly for the same `dir` while a walk is running (`done: false`,
/// numbers climbing) and exactly once with `done: true` when its subtree is
/// fully counted. A consumer keyed by `dir` therefore just overwrites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuUpdate {
    /// The directory these numbers are about.
    pub dir: PathBuf,
    /// Depth below the walk's root; the root itself is 0.
    pub depth: usize,
    /// See [`DuTotals::total_bytes`].
    pub total_bytes: u64,
    /// See [`DuTotals::apparent_bytes`].
    pub apparent_bytes: u64,
    /// See [`DuTotals::files`].
    pub files: u64,
    /// See [`DuTotals::dirs`].
    pub dirs: u64,
    /// Whether the subtree is finished. A `false` update is a running total:
    /// correct as far as it has counted, and only ever growing.
    pub done: bool,
    /// Names `read_dir` returned for **this directory alone**, capped at
    /// [`super::cache::MAX_STAMP_ENTRIES`] — not a recursive figure and not the
    /// same thing as `files`, which counts non-directories all the way down.
    ///
    /// It rides along because the walk gets it for free and the cache needs it:
    /// it is half of the cheap re-check that decides, on the way back into this
    /// directory, whether the remembered numbers are worth re-earning. Only a
    /// `done: true` update carries the finished count.
    pub entries: u64,
}

impl DuUpdate {
    fn new(dir: &Path, depth: usize, totals: &DuTotals, entries: u64, done: bool) -> DuUpdate {
        DuUpdate {
            dir: dir.to_path_buf(),
            depth,
            total_bytes: totals.total_bytes,
            apparent_bytes: totals.apparent_bytes,
            files: totals.files,
            dirs: totals.dirs,
            done,
            entries,
        }
    }

    /// The four numbers as a [`DuTotals`], for callers that store them.
    pub fn totals(&self) -> DuTotals {
        DuTotals {
            total_bytes: self.total_bytes,
            apparent_bytes: self.apparent_bytes,
            files: self.files,
            dirs: self.dirs,
        }
    }
}

/// What a walk is allowed to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuOptions {
    /// How far down updates are reported. Totals are always computed for the
    /// whole tree — this only bounds what is *told* to the caller, because a
    /// walk of a home directory would otherwise stream a million updates for
    /// directories nobody is looking at.
    ///
    /// `0` reports the root alone (enough to fill a size column for one
    /// directory); `1` reports the root and its children, which is what the
    /// drill-down and the heavy-hitter list need.
    pub depth_of_interest: usize,
    /// Follow the tree onto other mounted filesystems. Off by default; see the
    /// module essay.
    pub cross_filesystems: bool,
    /// Refuse to descend past this depth. See [`MAX_DEPTH`].
    pub max_depth: usize,
    /// Size of the hardlink dedup set. See [`MAX_HARDLINK_ENTRIES`].
    pub hardlink_cap: usize,
    /// Updates per batch. See [`DU_BATCH`].
    pub batch: usize,
    /// How often running totals are re-emitted. See [`UPDATE_INTERVAL`].
    pub update_interval: Duration,
    /// Count each immediate subdirectory's entries and report them *before* the
    /// walk starts. See [`child_counts`].
    pub count_children: bool,
    /// Take a subtree's total from the cache instead of descending into it,
    /// when the cache has a recent one and the directory still looks the same.
    ///
    /// **Off by default, and that is the safe direction.** This is what makes
    /// walking back up to a parent nearly free — the children were counted a
    /// moment ago and their answers are still sitting there — but it is an
    /// approximation twice over: the freshness test cannot see a change three
    /// levels down (see [`super::cache`]), and a file hardlinked between a
    /// reused subtree and a walked one is counted in both, because the reused
    /// side's inodes were never seen. The size column asks for it, and so does
    /// "what's big" mode ([`crate::du`]'s drill-down): both would rather have
    /// the answer on screen now than count again a subtree that was counted a
    /// minute ago and still looks the same. What neither is given is an
    /// estimate built on an estimate, because the records a reusing walk
    /// leaves behind are marked approximate and never reused in turn (see
    /// [`super::cache::DuCache::reusable_under`]).
    pub reuse_cache: bool,
}

impl Default for DuOptions {
    fn default() -> DuOptions {
        DuOptions {
            depth_of_interest: 1,
            cross_filesystems: false,
            max_depth: MAX_DEPTH,
            hardlink_cap: MAX_HARDLINK_ENTRIES,
            batch: DU_BATCH,
            update_interval: UPDATE_INTERVAL,
            count_children: false,
            reuse_cache: false,
        }
    }
}

impl DuOptions {
    /// Defaults, with the reporting depth set.
    pub fn at_depth(depth_of_interest: usize) -> DuOptions {
        DuOptions {
            depth_of_interest,
            ..DuOptions::default()
        }
    }

    /// The same, with the cheap child-count pass turned on — what the size
    /// column asks for, so a directory says `12 items` while its bytes are
    /// still being added up.
    pub fn counting_children(self) -> DuOptions {
        DuOptions {
            count_children: true,
            ..self
        }
    }

    /// The same, allowed to take fresh subtrees from the cache rather than
    /// re-counting them. See [`DuOptions::reuse_cache`].
    pub fn reusing_cache(self) -> DuOptions {
        DuOptions {
            reuse_cache: true,
            ..self
        }
    }
}

/// How many subdirectories one child-count pass will visit.
///
/// The pass is one `read_dir` per row, so its cost is linear in what is on
/// screen — and a screen holds a few dozen rows. 4,096 is far past any listing
/// a person is looking at and near enough to free; past it the walk's real
/// numbers are along shortly anyway, and a directory of 200,000 subdirectories
/// must not turn "enter a folder" into 200,000 syscalls.
pub const MAX_COUNTED_CHILDREN: usize = 4_096;

/// How far one subdirectory's own entries are counted before the answer
/// becomes "at least this many".
///
/// `read_dir(…).count()` walks the whole directory, and a `node_modules` or a
/// Maildir holds hundreds of thousands of names. Nobody reads "384,102 items"
/// as anything other than "a lot", so the pass stops at ten thousand and says
/// `10,000+` — which is the same information at a bounded cost, and keeps one
/// pathological row from delaying every other row's count behind it.
pub const MAX_COUNTED_ENTRIES: u64 = 10_000;

/// Subdirectories per batch of counts.
///
/// The counts exist to put a number on screen *before* the walk has added up a
/// byte, so they must not queue up behind the slowest directory in the listing:
/// 16 rows is most of a screenful, and emitting at that granularity means the
/// first rows appear while the rest are still being counted.
pub const COUNT_BATCH: usize = 16;

/// One subdirectory's entry count, and whether the counting stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildCount {
    /// Names seen, up to [`MAX_COUNTED_ENTRIES`].
    pub entries: u64,
    /// The cap was hit, so `entries` is a floor rather than the answer.
    pub capped: bool,
}

/// How many immediate entries each of `root`'s subdirectories holds, streamed
/// to `emit` in batches of [`COUNT_BATCH`].
///
/// **The number that is on screen before the walk has counted a byte.** A
/// recursive size takes seconds on a big tree; `read_dir` of one directory
/// takes microseconds, and "12 items" is a true, useful thing to say in the
/// meantime — where an em dash says nothing and a `0 B` would be a lie.
///
/// Deliberately *not* recursive and deliberately not stat'ing anything beyond
/// what the boundary check needs: this counts names, which is all `read_dir`
/// hands over and all the answer claims. Symlinks to directories are skipped
/// for the same reason [`walk`] skips them — the listing shows them as links,
/// and following one here would count a tree that is somewhere else — and a
/// subdirectory on another filesystem is skipped for the same reason the walk
/// will not descend into it, unless [`DuOptions::cross_filesystems`] says
/// otherwise: a `read_dir` of an NFS mount is not a cheap pass.
///
/// Batches arrive in `read_dir` order. A subdirectory that could not be read is
/// simply absent: an unreadable folder has no honest count, and inventing a
/// zero would be worse than the dash it replaces.
pub fn child_counts(
    root: &Path,
    options: &DuOptions,
    cancelled: &dyn Fn() -> bool,
    emit: &mut dyn FnMut(Vec<(PathBuf, ChildCount)>),
) {
    let Ok(root_meta) = std::fs::symlink_metadata(root) else {
        return;
    };
    let root_dev = crate::platform::meta::dev(&root_meta);
    let Ok(reader) = std::fs::read_dir(root) else {
        return;
    };
    let mut batch: Vec<(PathBuf, ChildCount)> = Vec::new();
    let mut visited = 0usize;
    for item in reader.flatten() {
        if visited >= MAX_COUNTED_CHILDREN || cancelled() {
            break;
        }
        // `file_type` on a `DirEntry` comes from the `d_type` the kernel
        // already returned, so this is free on every filesystem that fills it
        // in and one `lstat` on the ones that do not.
        if !item.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        // The boundary check the walk makes, made here too: without it the
        // cheap pass is the one thing in the mode that wanders onto the backup
        // drive under `/mnt`, and it does so *before* the walk has started.
        if let Ok(meta) = item.metadata() {
            if crosses_boundary(
                root_dev,
                crate::platform::meta::dev(&meta),
                options.cross_filesystems,
            ) {
                continue;
            }
        }
        visited += 1;
        let path = item.path();
        let Ok(children) = std::fs::read_dir(&path) else {
            continue;
        };
        let mut entries = 0u64;
        let mut capped = false;
        for _ in children {
            // Checked per name, not per directory: one `read_dir` of a
            // Maildir is millions of names, and a pass that only looked
            // between directories would hold a cancelled walk open for all of
            // them.
            if cancelled() {
                return;
            }
            entries += 1;
            if entries >= MAX_COUNTED_ENTRIES {
                capped = true;
                break;
            }
        }
        batch.push((path, ChildCount { entries, capped }));
        if batch.len() >= COUNT_BATCH {
            emit(std::mem::take(&mut batch));
        }
    }
    if !batch.is_empty() {
        emit(batch);
    }
}

/// Whether an entry on device `child_dev` is off-limits for a walk rooted on
/// `root_dev`.
///
/// Split out and public so the boundary rule can be tested without a second
/// filesystem to hand — mounting one is not something a unit test may do, and
/// "the flag is wired to the comparison" is the part that can actually break.
pub fn crosses_boundary(root_dev: u64, child_dev: u64, cross_filesystems: bool) -> bool {
    !cross_filesystems && root_dev != child_dev
}

/// Block usage and apparent size of one already-`stat`ed thing.
fn sizes_of(meta: &std::fs::Metadata) -> (u64, u64) {
    (crate::platform::meta::blocks_bytes(meta), meta.len())
}

struct Frame {
    dir: PathBuf,
    depth: usize,
    reader: std::fs::ReadDir,
    totals: DuTotals,
    /// Names this directory's own `read_dir` has handed over, capped. Counted
    /// before any filtering, so it means the same thing as
    /// [`super::cache::count_names`] — the two only have to agree with each
    /// other.
    entries: u64,
}

/// What a walk may take from the cache instead of descending.
///
/// Asked once per directory, with the `mtime` the walk has already `stat`ed so
/// the answer costs no extra syscall on a miss. A `Some` is the subtree's
/// totals and the name count they were stamped with; the walk folds the totals
/// into the parent and steps over the whole tree.
pub type KnownSubtree<'a> = dyn Fn(&Path, Option<SystemTime>) -> Option<(DuTotals, u64)> + 'a;

/// Nothing is known: what [`walk`] passes for callers that want an exact count.
fn nothing_known(_: &Path, _: Option<SystemTime>) -> Option<(DuTotals, u64)> {
    None
}

fn flush(pending: &mut Vec<DuUpdate>, emit: &mut dyn FnMut(Vec<DuUpdate>)) {
    if !pending.is_empty() {
        emit(std::mem::take(pending));
    }
}

/// Queue a running total for every open directory shallow enough to be of
/// interest. At most `depth_of_interest + 1` updates, however deep the walk is.
fn push_partials(stack: &[Frame], depth_of_interest: usize, pending: &mut Vec<DuUpdate>) {
    for frame in stack.iter().take(depth_of_interest + 1) {
        pending.push(DuUpdate::new(
            &frame.dir,
            frame.depth,
            &frame.totals,
            frame.entries,
            false,
        ));
    }
}

/// Walk `root`, streaming per-directory totals to `emit` and stopping when
/// `cancelled` says so.
///
/// Returns the root's totals. `cancelled` is polled at every directory boundary
/// and every [`CANCEL_CHECK_ENTRIES`] entries; when it fires the walk gives up
/// where it stands and returns [`DfError::Cancelled`], having already emitted
/// everything it had finished. A root that cannot be opened is an
/// [`DfError::Io`]; an unreadable directory *inside* the tree is not — it is
/// counted as one directory, logged, and stepped over, because "you cannot see
/// inside `/root`" must not stop a scan of `/`.
pub fn walk(
    root: &Path,
    options: &DuOptions,
    cancelled: &dyn Fn() -> bool,
    emit: &mut dyn FnMut(Vec<DuUpdate>),
) -> Result<DuTotals> {
    walk_reusing(root, options, cancelled, &nothing_known, emit)
}

/// [`walk`], with a cache to lean on.
///
/// `known` is consulted for every directory the walk is about to descend into,
/// and only when [`DuOptions::reuse_cache`] is set. When it answers, that
/// subtree is folded in whole and never opened: the walk of a parent you have
/// just come up from is then a `read_dir` of one level plus a handful of
/// hashmap lookups, which is the difference between "the numbers are there" and
/// "the numbers arrive in four seconds".
///
/// A reused directory is still *reported* — one `done: true` update at its
/// depth, carrying the remembered totals — so a consumer cannot tell the
/// difference and the caller's cache-writing path sees the same shape it always
/// did.
pub fn walk_reusing(
    root: &Path,
    options: &DuOptions,
    cancelled: &dyn Fn() -> bool,
    known: &KnownSubtree<'_>,
    emit: &mut dyn FnMut(Vec<DuUpdate>),
) -> Result<DuTotals> {
    let root_meta = std::fs::symlink_metadata(root).map_err(|e| DfError::io(root, e))?;
    if !root_meta.is_dir() {
        return Err(DfError::io(
            root,
            std::io::Error::new(std::io::ErrorKind::NotADirectory, "not a directory"),
        ));
    }
    let root_dev = crate::platform::meta::dev(&root_meta);
    let root_reader = std::fs::read_dir(root).map_err(|e| DfError::io(root, e))?;

    let (blocks, apparent) = sizes_of(&root_meta);
    let mut stack = vec![Frame {
        dir: root.to_path_buf(),
        depth: 0,
        reader: root_reader,
        totals: DuTotals {
            total_bytes: blocks,
            apparent_bytes: apparent,
            files: 0,
            dirs: 1,
        },
        entries: 0,
    }];

    let mut seen_links: HashSet<(u64, u64)> = HashSet::new();
    let mut link_cap_hit = false;
    let mut depth_cap_hit = false;
    let mut pending: Vec<DuUpdate> = Vec::new();
    let mut since_check = 0usize;
    let mut last_partial = Instant::now();
    let mut root_totals = DuTotals::default();

    loop {
        since_check += 1;
        if since_check >= CANCEL_CHECK_ENTRIES {
            since_check = 0;
            if cancelled() {
                return Err(DfError::Cancelled);
            }
        }
        if last_partial.elapsed() >= options.update_interval {
            last_partial = Instant::now();
            push_partials(&stack, options.depth_of_interest, &mut pending);
            flush(&mut pending, emit);
        }

        let next = match stack.last_mut() {
            Some(frame) => frame.reader.next(),
            None => break,
        };

        // Counted before anything is filtered — a symlink and a file on another
        // filesystem are both names `read_dir` returned, and the stamp this
        // feeds is compared against a plain `read_dir` count.
        if let Some(frame) = stack.last_mut() {
            if next.is_some() && frame.entries < super::cache::MAX_STAMP_ENTRIES {
                frame.entries += 1;
            }
        }

        let item = match next {
            Some(Ok(item)) => item,
            // One entry that vanished mid-walk, or a `readdir` that failed
            // partway. The rest of the directory is still worth counting.
            Some(Err(_)) => continue,
            None => {
                // The directory is finished: fold it into its parent and say so.
                let Some(frame) = stack.pop() else { break };
                if frame.depth <= options.depth_of_interest {
                    pending.push(DuUpdate::new(
                        &frame.dir,
                        frame.depth,
                        &frame.totals,
                        frame.entries,
                        true,
                    ));
                    if pending.len() >= options.batch {
                        flush(&mut pending, emit);
                    }
                }
                let Some(parent) = stack.last_mut() else {
                    root_totals = frame.totals;
                    break;
                };
                parent.totals.add(&frame.totals);
                if cancelled() {
                    return Err(DfError::Cancelled);
                }
                continue;
            }
        };

        // `symlink_metadata`, not `metadata`: a symlink must be recognised as
        // one before anything follows it, and nothing here ever does.
        let Ok(meta) = item.metadata() else { continue };
        if meta.is_symlink() {
            continue;
        }
        if crosses_boundary(
            root_dev,
            crate::platform::meta::dev(&meta),
            options.cross_filesystems,
        ) {
            continue;
        }

        if meta.is_dir() {
            let path = item.path();
            let depth = stack.last().map(|f| f.depth + 1).unwrap_or(1);
            let (blocks, apparent) = sizes_of(&meta);
            let own = DuTotals {
                total_bytes: blocks,
                apparent_bytes: apparent,
                files: 0,
                dirs: 1,
            };
            // The cache, before the `read_dir`. A subtree somebody walked a
            // minute ago and has not touched since is added in one line here
            // instead of a hundred thousand `stat`s below.
            if options.reuse_cache {
                if let Some((totals, entries)) = known(&path, meta.modified().ok()) {
                    if let Some(parent) = stack.last_mut() {
                        parent.totals.add(&totals);
                    }
                    if depth <= options.depth_of_interest {
                        pending.push(DuUpdate::new(&path, depth, &totals, entries, true));
                        if pending.len() >= options.batch {
                            flush(&mut pending, emit);
                        }
                    }
                    continue;
                }
            }
            if depth > options.max_depth {
                if !depth_cap_hit {
                    depth_cap_hit = true;
                    log::warn!(
                        "du: {} is deeper than {} levels; not descending further",
                        path.display(),
                        options.max_depth
                    );
                }
                if let Some(parent) = stack.last_mut() {
                    parent.totals.add(&own);
                }
                continue;
            }
            match std::fs::read_dir(&path) {
                Ok(reader) => stack.push(Frame {
                    dir: path,
                    depth,
                    reader,
                    totals: own,
                    entries: 0,
                }),
                Err(e) => {
                    // Unreadable, not absent: it exists and occupies an inode,
                    // so it counts as one directory with nothing visible in it.
                    log::debug!("du: {}: {e}", path.display());
                    if let Some(parent) = stack.last_mut() {
                        parent.totals.add(&own);
                    }
                }
            }
            continue;
        }

        let (blocks, apparent) = sizes_of(&meta);
        let counted = if crate::platform::meta::nlink(&meta) > 1 {
            if seen_links.len() >= options.hardlink_cap {
                if !link_cap_hit {
                    link_cap_hit = true;
                    log::warn!(
                        "du: more than {} hardlinked files under {}; totals past here \
                         double-count shared content",
                        options.hardlink_cap,
                        root.display()
                    );
                }
                true
            } else {
                seen_links.insert((
                    crate::platform::meta::dev(&meta),
                    crate::platform::meta::ino(&meta),
                ))
            }
        } else {
            true
        };

        if let Some(frame) = stack.last_mut() {
            frame.totals.files = frame.totals.files.saturating_add(1);
            if counted {
                frame.totals.total_bytes = frame.totals.total_bytes.saturating_add(blocks);
                frame.totals.apparent_bytes = frame.totals.apparent_bytes.saturating_add(apparent);
            }
        }
    }

    flush(&mut pending, emit);
    Ok(root_totals)
}

/// Walk on the calling thread and return only the root's totals.
///
/// For tests and for the rare synchronous caller; anything the user is waiting
/// on goes through [`super::scanner::DuScanner`] instead.
pub fn walk_blocking(root: &Path, options: &DuOptions) -> Result<DuTotals> {
    let never = || false;
    let mut sink = |_: Vec<DuUpdate>| {};
    walk(root, options, &never, &mut sink)
}
