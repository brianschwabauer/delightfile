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
use std::time::{Duration, Instant};

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;

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
}

impl DuUpdate {
    fn new(dir: &Path, depth: usize, totals: &DuTotals, done: bool) -> DuUpdate {
        DuUpdate {
            dir: dir.to_path_buf(),
            depth,
            total_bytes: totals.total_bytes,
            apparent_bytes: totals.apparent_bytes,
            files: totals.files,
            dirs: totals.dirs,
            done,
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
    (meta.blocks().saturating_mul(BLOCK_UNIT), meta.size())
}

struct Frame {
    dir: PathBuf,
    depth: usize,
    reader: std::fs::ReadDir,
    totals: DuTotals,
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
        pending.push(DuUpdate::new(&frame.dir, frame.depth, &frame.totals, false));
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
    let root_meta = std::fs::symlink_metadata(root).map_err(|e| DfError::io(root, e))?;
    if !root_meta.is_dir() {
        return Err(DfError::io(
            root,
            std::io::Error::new(std::io::ErrorKind::NotADirectory, "not a directory"),
        ));
    }
    let root_dev = root_meta.dev();
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

        let item = match next {
            Some(Ok(item)) => item,
            // One entry that vanished mid-walk, or a `readdir` that failed
            // partway. The rest of the directory is still worth counting.
            Some(Err(_)) => continue,
            None => {
                // The directory is finished: fold it into its parent and say so.
                let Some(frame) = stack.pop() else { break };
                if frame.depth <= options.depth_of_interest {
                    pending.push(DuUpdate::new(&frame.dir, frame.depth, &frame.totals, true));
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
        if crosses_boundary(root_dev, meta.dev(), options.cross_filesystems) {
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
        let counted = if meta.nlink() > 1 {
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
                seen_links.insert((meta.dev(), meta.ino()))
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
