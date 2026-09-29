//! The worker pool that runs walks in the background.
//!
//! Structurally this is [`crate::fs::Scanner`]'s twin, and deliberately
//! so: a request channel a small pool shares, a monotonic token per request, a
//! live-token map that doubles as the cancel switch, results on a crossbeam
//! channel, and a [`Notifier`] callback to ring the app's bell because df-core
//! is not allowed to know what the app waits on (PLAN §1). Anyone who has read
//! `fs/scan.rs` already knows how this file works; the differences are the
//! interesting part.
//!
//! **A du request is long.** A directory scan is bounded by one `read_dir`; a du
//! of `~` is minutes of syscalls. So supersede matters more (arrowing through
//! directories with the size column on would otherwise pile up walks that will
//! never be looked at), the cancellation check has to reach inside the walk
//! rather than sitting between batches of it, and the results have to stream —
//! see [`fn@super::walk`], which does all three.
//!
//! **The answer is worth keeping.** A directory scan is cheap enough to redo;
//! a du is not, so every completed walk lands in a [`DuCache`] the scanner owns
//! and [`DuScanner::heavy_hitters`] reads. That cache is the "what's big" mode's
//! actual data structure — the message stream is only how it fills up live.
//!
//! **The workers step aside.** A du of `~` is a thread issuing `stat` as fast
//! as the kernel will answer, and on a busy machine it is scheduled against the
//! UI thread as an equal. Each worker nices itself to
//! [`crate::thread::NICE_BULK`] on the way in, so the sizes arrive a moment
//! later on a loaded laptop and never at the cost of a frame (PLAN §1).
//!
//! **Two workers, not three.** These threads are not waiting on a slow mount;
//! they are issuing `stat` back to back, and past two of them a du of one tree
//! is competing with itself for the same page cache and the same disk queue.
//! Two covers the real concurrency: the tree you are looking at, and the one you
//! just left that has not noticed yet.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::fs::Notifier;
use crate::{DfError, Result};

use super::cache::{
    count_names, current_mtime, ChildTotal, DirStamp, DuCache, DuRecord, HeavyHitter, Remembered,
};
use super::walk::{child_counts, walk_reusing, ChildCount, DuOptions, DuTotals, DuUpdate};

/// Walk workers. See the module essay.
pub const DU_WORKERS: usize = 2;

/// How many directories one walk will remember well enough to cache.
///
/// The walk streams updates for every directory within `depth_of_interest`, and
/// the scanner keeps them so it can build cache records at the end. That is a
/// map that grows with the tree's shape rather than with a fixed budget, so it
/// gets a cap: 65,536 directories is ~10 MB of paths and totals, and is more
/// subdirectories than any directory a person is drilling into actually has.
///
/// Past the cap the walk keeps *counting* — totals stay correct — but stops
/// recording new directories, and the resulting records are marked
/// [`DuRecord::children_complete`]`= false`. A heavy-hitter list built from such
/// a record can miss a large child that happened to be late in `readdir` order.
/// The cap is here so a pathological tree costs accuracy in a flagged field
/// rather than memory without a limit.
pub const MAX_TRACKED_DIRS: usize = 65_536;

/// Identifies one walk. Monotonic per [`DuScanner`], never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DuToken(pub u64);

/// What a walk sends back.
#[derive(Debug)]
pub enum DuMessage {
    /// The walk began. Sent before any numbers, so the UI can put up an empty
    /// drill-down at the moment it knows one is coming.
    Started { token: DuToken, root: PathBuf },
    /// How many entries each immediate subdirectory holds, from the cheap
    /// `read_dir` pass — sent in batches, before the walk, when
    /// [`DuOptions::count_children`] asked for it. The size column's first
    /// honest answer; see [`fn@super::walk::child_counts`].
    ///
    /// Batched rather than sent once at the end of the pass: one directory of
    /// a hundred thousand names must not hold every other row's number behind
    /// it.
    Counts {
        token: DuToken,
        root: PathBuf,
        counts: Vec<(PathBuf, ChildCount)>,
    },
    /// Per-directory totals. Some are running (`done: false`) and some final;
    /// a consumer keyed by [`DuUpdate::dir`] just overwrites.
    Progress {
        token: DuToken,
        root: PathBuf,
        updates: Vec<DuUpdate>,
    },
    /// Everything is counted and the cache has the answer.
    Done {
        token: DuToken,
        root: PathBuf,
        totals: DuTotals,
    },
    /// The root could not be walked. Not sent for cancellation — a cancelled
    /// walk goes quiet, because nobody is waiting for its answer by definition.
    Failed {
        token: DuToken,
        root: PathBuf,
        error: DfError,
    },
}

impl DuMessage {
    pub fn token(&self) -> DuToken {
        match self {
            DuMessage::Started { token, .. }
            | DuMessage::Counts { token, .. }
            | DuMessage::Progress { token, .. }
            | DuMessage::Done { token, .. }
            | DuMessage::Failed { token, .. } => *token,
        }
    }

    pub fn root(&self) -> &Path {
        match self {
            DuMessage::Started { root, .. }
            | DuMessage::Counts { root, .. }
            | DuMessage::Progress { root, .. }
            | DuMessage::Done { root, .. }
            | DuMessage::Failed { root, .. } => root,
        }
    }
}

struct Request {
    token: DuToken,
    root: PathBuf,
    options: DuOptions,
}

/// One walk's cancel switch: `false` the moment it is superseded, cancelled
/// or finished.
///
/// A flag per token rather than a `contains_key` on the map, because the walk
/// asks this question **once per directory entry** — millions of times on a
/// home directory — and a mutex on the path there is a lock every worker and
/// the UI thread contend for. An atomic load is a load.
type Alive = Arc<AtomicBool>;

type Live = Arc<Mutex<HashMap<DuToken, (PathBuf, Alive)>>>;
type Shared = Arc<Mutex<DuCache>>;

/// The pool. Dropping it cancels everything in flight, closes the request
/// channel and joins the workers, so a test cannot outlive its threads.
pub struct DuScanner {
    requests: Option<Sender<Request>>,
    updates: Receiver<DuMessage>,
    live: Live,
    cache: Shared,
    next_token: AtomicU64,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl DuScanner {
    /// Start `workers` threads. `notify` is rung once per message sent.
    pub fn new(workers: usize, notify: Notifier) -> DuScanner {
        let (req_tx, req_rx) = unbounded::<Request>();
        let (up_tx, up_rx) = unbounded::<DuMessage>();
        let live: Live = Arc::new(Mutex::new(HashMap::new()));
        let cache: Shared = Arc::new(Mutex::new(DuCache::new()));

        let mut handles = Vec::with_capacity(workers);
        for i in 0..workers.max(1) {
            let req_rx = req_rx.clone();
            let up_tx = up_tx.clone();
            let live = Arc::clone(&live);
            let cache = Arc::clone(&cache);
            let notify = Arc::clone(&notify);
            let handle = std::thread::Builder::new()
                .name(format!("df-du-{i}"))
                .spawn(move || {
                    // Before the first request, so it costs one syscall per
                    // worker for the life of the session.
                    crate::thread::lower_priority(crate::thread::NICE_BULK);
                    for request in req_rx {
                        run_walk(request, &up_tx, &live, &cache, &notify);
                    }
                });
            match handle {
                // A worker that will not spawn is not fatal: the other one still
                // answers, and with none the size column simply stays blank.
                Err(e) => log::warn!("du worker {i} did not start: {e}"),
                Ok(h) => handles.push(h),
            }
        }

        DuScanner {
            requests: Some(req_tx),
            updates: up_rx,
            live,
            cache,
            next_token: AtomicU64::new(1),
            workers: handles,
        }
    }

    /// A scanner with the default worker count.
    pub fn start(notify: Notifier) -> DuScanner {
        DuScanner::new(DU_WORKERS, notify)
    }

    /// Queue a walk of `root`, reporting per-directory totals down to
    /// `depth_of_interest`. Any in-flight walk of the same root is cancelled
    /// first, so a rescan-on-change is just another `request`.
    ///
    /// `depth_of_interest` of 1 is the useful default: it fills the size column
    /// for every directory in the listing and feeds
    /// [`DuScanner::heavy_hitters`] in one pass.
    pub fn request(&self, root: impl Into<PathBuf>, depth_of_interest: usize) -> DuToken {
        self.request_with(root, DuOptions::at_depth(depth_of_interest))
    }

    /// [`DuScanner::request`] with the walk's rules spelled out — crossing
    /// filesystems, depth and hardlink caps, batch timing.
    pub fn request_with(&self, root: impl Into<PathBuf>, options: DuOptions) -> DuToken {
        let root = root.into();
        let token = DuToken(self.next_token.fetch_add(1, Ordering::Relaxed));
        {
            let mut live = lock(&self.live);
            // Superseding is cancelling: the flag has to be lowered, not just
            // the entry dropped, or the walk it belonged to reads a flag
            // nobody can reach any more and runs to completion.
            live.retain(|_, (r, alive)| {
                if *r == root {
                    alive.store(false, Ordering::Relaxed);
                    return false;
                }
                true
            });
            live.insert(token, (root.clone(), Arc::new(AtomicBool::new(true))));
        }
        if let Some(requests) = &self.requests {
            let request = Request {
                token,
                root,
                options,
            };
            // Unbounded channel with receivers that only die with the pool, so
            // a send failure means the pool is already gone.
            if requests.send(request).is_err() {
                stand_down(&self.live, token);
            }
        }
        token
    }

    /// Stop a walk. Messages already in the channel may still arrive; the
    /// consumer drops them by token.
    pub fn cancel(&self, token: DuToken) {
        stand_down(&self.live, token);
    }

    /// Stop everything in flight (leaving the mode, closing the tab, quitting).
    pub fn cancel_all(&self) {
        for (_, (_, alive)) in lock(&self.live).drain() {
            alive.store(false, Ordering::Relaxed);
        }
    }

    pub fn is_live(&self, token: DuToken) -> bool {
        lock(&self.live).contains_key(&token)
    }

    /// The channel, for a caller that wants to select on it.
    pub fn updates(&self) -> &Receiver<DuMessage> {
        &self.updates
    }

    /// Everything that has arrived, without blocking. What the app calls when
    /// its wake event fires.
    pub fn drain(&self) -> Vec<DuMessage> {
        self.updates.try_iter().collect()
    }

    /// How long a walk's answer is served before it is re-earned.
    pub fn ttl(&self) -> Duration {
        lock(&self.cache).ttl()
    }

    /// Set that lifetime — `[mgr] folder_size_ttl`, in practice.
    pub fn set_ttl(&self, ttl: Duration) {
        lock(&self.cache).set_ttl(ttl);
    }

    /// A directory's remembered totals, if they were walked inside the TTL.
    /// See [`DuCache::fresh`] — and the module essay's warning that recent is
    /// not the same thing as true.
    pub fn cached(&self, dir: &Path) -> Option<DuRecord> {
        lock(&self.cache).fresh(dir, Instant::now()).cloned()
    }

    /// What is remembered about `dir` and whether it still needs a walk.
    ///
    /// The question the size column asks on entering a directory: a `fresh`
    /// answer is drawn and nothing is started, a stale one is drawn wearing its
    /// `~` while a walk corrects it behind it.
    pub fn remembered(&self, dir: &Path) -> Option<Remembered> {
        lock(&self.cache).remembered(dir, Instant::now())
    }

    /// Every immediate child of `dir` the cache can already put a number
    /// against. See [`DuCache::remembered_children`] — this is what makes
    /// stepping back up to a parent instant.
    pub fn remembered_children(&self, dir: &Path) -> Vec<ChildTotal> {
        lock(&self.cache).remembered_children(dir, Instant::now())
    }

    /// The "what's big" list for `dir`: children by disk usage, largest first,
    /// with each one's share of the parent. Empty until a walk of `dir` with
    /// `depth_of_interest >= 1` has finished.
    ///
    /// Drilling into a child returns empty on the first press — the child's
    /// *total* is known from the parent's walk, but its children are not — so
    /// the caller pairs this with a `request` for the child and lets the stream
    /// fill it in. That second walk is nearly free: the metadata it needs is the
    /// metadata the first walk just pulled into the page cache.
    pub fn heavy_hitters(&self, dir: &Path) -> Vec<HeavyHitter> {
        lock(&self.cache).heavy_hitters(dir, Instant::now())
    }

    /// Fill in [`crate::fs::Entry::len`] for the directories in a listing from
    /// the cache, and report how many rows were filled.
    ///
    /// `Entry::len` is deliberately zero for directories (see its docs) because
    /// nothing cheap knows the answer. This is the expensive thing that does:
    /// after a walk of `dir` the size sort and the size linemode order
    /// directories by what is actually in them. Rows with no cached child stay
    /// at zero, which is the same "not known yet" they started as.
    pub fn fill_dir_sizes(&self, dir: &Path, entries: &mut [crate::fs::Entry]) -> usize {
        let mut cache = lock(&self.cache);
        let Some(record) = cache.fresh(dir, Instant::now()) else {
            return 0;
        };
        let sizes: HashMap<&str, u64> = record
            .children
            .iter()
            .map(|(name, totals)| (name.as_str(), totals.total_bytes))
            .collect();
        let mut filled = 0;
        for entry in entries.iter_mut() {
            if !entry.is_dir() {
                continue;
            }
            if let Some(bytes) = sizes.get(entry.name.as_str()) {
                entry.len = *bytes;
                filled += 1;
            }
        }
        filled
    }

    /// Forget one directory's record, so the next request re-walks it. The
    /// "refresh" key, and the answer to the cache's mtime blindness.
    pub fn forget(&self, dir: &Path) -> bool {
        lock(&self.cache).forget(dir)
    }

    pub fn clear_cache(&self) {
        lock(&self.cache).clear();
    }

    /// How many directories the cache is holding. For tests and a debug
    /// overlay; nothing about behaviour depends on it.
    pub fn cache_len(&self) -> usize {
        lock(&self.cache).len()
    }
}

impl Drop for DuScanner {
    fn drop(&mut self) {
        self.cancel_all();
        self.requests = None;
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

/// Lock without ever panicking on a poisoned mutex.
///
/// Poisoning means another thread panicked while holding this lock. Behind it
/// are a token map and a size cache — no invariant a panic could leave half
/// applied — so carrying on is strictly better than taking the file manager down
/// over a stale byte count.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drop one walk from the live map and lower its flag, in that order.
fn stand_down(live: &Live, token: DuToken) {
    if let Some((_, alive)) = lock(live).remove(&token) {
        alive.store(false, Ordering::Relaxed);
    }
}

fn run_walk(
    request: Request,
    updates: &Sender<DuMessage>,
    live: &Live,
    cache: &Shared,
    notify: &Notifier,
) {
    let Request {
        token,
        root,
        options,
    } = request;
    // Cancelled before a worker picked it up — the common case when moving
    // through directories faster than a walk can finish.
    let Some(alive) = lock(live).get(&token).map(|(_, alive)| Arc::clone(alive)) else {
        return;
    };
    let cancelled = || !alive.load(Ordering::Relaxed);

    let send = |message: DuMessage| -> bool {
        let ok = updates.send(message).is_ok();
        if ok {
            notify();
        }
        ok
    };

    if !send(DuMessage::Started {
        token,
        root: root.clone(),
    }) {
        return;
    }

    // The cheap pass first: one `read_dir` per subdirectory, so the size column
    // has something true to say within a frame or two of entering a directory
    // rather than after the whole subtree has been added up.
    if options.count_children {
        let mut open = true;
        let mut emit = |counts: Vec<(PathBuf, ChildCount)>| {
            if open {
                open = send(DuMessage::Counts {
                    token,
                    root: root.clone(),
                    counts,
                });
            }
        };
        child_counts(&root, &options, &cancelled, &mut emit);
        if !open {
            return;
        }
    }

    // Read before the walk, not after: a directory changed *during* the walk
    // must invalidate the record, and stamping it with the mtime we saw first
    // is what makes that happen.
    let root_mtime = current_mtime(&root);
    let started = Instant::now();

    // One snapshot of what the cache already knows about this tree, taken under
    // the lock once instead of a lock per directory the walk meets. Empty when
    // the caller asked for an exact count.
    let reusable = if options.reuse_cache {
        lock(cache).reusable_under(&root, started)
    } else {
        HashMap::new()
    };
    // Which of them the walk actually took. Their records must keep the
    // `walked_at` they already have: re-stamping a subtree that was skipped
    // rather than counted would let a tree stay "recent" forever by being
    // walked past every few minutes, which is the one thing the TTL is for.
    let reused: Mutex<HashSet<PathBuf>> = Mutex::new(HashSet::new());
    let known = |dir: &Path, mtime: Option<std::time::SystemTime>| -> Option<(DuTotals, u64)> {
        let (stamp, totals) = reusable.get(crate::path::key(dir).as_ref())?;
        // The `mtime` the walk already has, and one `read_dir` — which is the
        // syscall descending would have cost anyway, so a hit is free and a
        // miss costs the price of the check alone.
        let live = DirStamp {
            mtime,
            entries: count_names(dir),
        };
        if !stamp.agrees_with(&live) {
            return None;
        }
        lock(&reused).insert(dir.to_path_buf());
        Some((*totals, stamp.entries.unwrap_or(0)))
    };

    let mut tracked: HashMap<PathBuf, (DuTotals, u64)> = HashMap::new();
    let mut tracking_complete = true;
    let mut channel_open = true;

    {
        let mut emit = |batch: Vec<DuUpdate>| {
            for update in &batch {
                if !update.done {
                    continue;
                }
                if tracked.len() >= MAX_TRACKED_DIRS && !tracked.contains_key(&update.dir) {
                    if tracking_complete {
                        tracking_complete = false;
                        log::warn!(
                            "du: more than {MAX_TRACKED_DIRS} directories under {}; \
                             the size list may be missing entries",
                            root.display()
                        );
                    }
                    continue;
                }
                tracked.insert(update.dir.clone(), (update.totals(), update.entries));
            }
            if channel_open {
                channel_open = send(DuMessage::Progress {
                    token,
                    root: root.clone(),
                    updates: batch,
                });
            }
        };

        match walk_reusing(&root, &options, &cancelled, &known, &mut emit) {
            Ok(totals) => {
                stand_down(live, token);
                let reused = lock(&reused);
                // **What this walk leaves behind is only as good as what it
                // took.** A walk that folded in even one cached subtree
                // produced totals it did not count, and every record it writes
                // may sit above that subtree — so they all go in marked, and
                // the callers that need a counted number ask for their own
                // walk. A reusing walk that found nothing to reuse counted
                // everything and is exact.
                let approximate = !reused.is_empty();
                store(
                    cache,
                    &root,
                    root_mtime,
                    Instant::now(),
                    totals,
                    &tracked,
                    &reused,
                    tracking_complete,
                    approximate,
                );
                send(DuMessage::Done {
                    token,
                    root: root.clone(),
                    totals,
                });
            }
            // Cancellation is not a failure and gets no message: the only
            // caller who could care has already stopped listening.
            Err(DfError::Cancelled) => {}
            Err(error) => {
                stand_down(live, token);
                send(DuMessage::Failed {
                    token,
                    root: root.clone(),
                    error,
                });
            }
        }
    }
}

/// Turn a finished walk's per-directory totals into cache records.
///
/// One record per tracked directory, each carrying the children it has among
/// the other tracked directories — so a `depth_of_interest` of 1 produces a
/// record for the root with all its subdirectories attached, plus a bare record
/// per subdirectory that later makes its size column instant even though its own
/// children were never counted.
///
/// **That per-subdirectory record is what makes going up cheap.** Walking into
/// `~/Work/delightfile` leaves one behind for it; stepping back out to `~/Work`
/// finds the total already computed, and the walk of `~/Work` steps over the
/// whole subtree rather than counting it again.
///
/// Directories in `reused` are left exactly as they are: their numbers came out
/// of the cache in the first place, and rewriting them would move their
/// `walked_at` forward without anything having been counted.
///
/// `approximate` says whether this walk reused anything at all, and every
/// record it writes carries it — see [`DuRecord::approximate`].
///
/// Only directories are children here. A big *file* is a heavy hitter too, but
/// its size is already in the listing (`Entry::len` is honest for files), so
/// making the walk carry file names as well would double the memory to
/// re-derive something the caller has.
#[allow(clippy::too_many_arguments)]
fn store(
    cache: &Shared,
    root: &Path,
    root_mtime: Option<std::time::SystemTime>,
    walked_at: Instant,
    totals: DuTotals,
    tracked: &HashMap<PathBuf, (DuTotals, u64)>,
    reused: &HashSet<PathBuf>,
    complete: bool,
    approximate: bool,
) {
    let mut children: HashMap<&Path, Vec<(String, DuTotals)>> = HashMap::new();
    for (dir, (dir_totals, _)) in tracked {
        if dir == root {
            continue;
        }
        let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
            continue;
        };
        children
            .entry(parent)
            .or_default()
            .push((name.to_string_lossy().into_owned(), *dir_totals));
    }

    let mut cache = lock(cache);
    for (dir, (dir_totals, entries)) in tracked {
        if reused.contains(dir) {
            continue;
        }
        let kids = children.remove(dir.as_path()).unwrap_or_default();
        let mtime = if dir == root {
            root_mtime
        } else {
            current_mtime(dir)
        };
        let stamp = DirStamp {
            mtime,
            entries: Some(*entries),
        };
        insert(
            &mut cache,
            dir,
            stamp,
            walked_at,
            *dir_totals,
            kids,
            complete,
            approximate,
        );
    }
    // A `depth_of_interest` of 0 emits nothing for the root's children, and a
    // root with no subdirectories tracks only itself; either way the root's own
    // totals must still land.
    if !tracked.contains_key(root) {
        let stamp = DirStamp {
            mtime: root_mtime,
            entries: count_names(root),
        };
        insert(
            &mut cache,
            root,
            stamp,
            walked_at,
            totals,
            Vec::new(),
            complete,
            approximate,
        );
    }
}

/// One record, through whichever of the cache's two doors this walk earned.
#[allow(clippy::too_many_arguments)]
fn insert(
    cache: &mut DuCache,
    dir: impl Into<PathBuf>,
    stamp: DirStamp,
    walked_at: Instant,
    totals: DuTotals,
    children: Vec<(String, DuTotals)>,
    complete: bool,
    approximate: bool,
) {
    if approximate {
        cache.insert_approximate(dir, stamp, walked_at, totals, children, complete);
    } else {
        cache.insert(dir, stamp, walked_at, totals, children, complete);
    }
}

/// Walk one tree on the calling thread, ignoring the stream.
///
/// The synchronous door into the same machinery, for tests and for a caller that
/// has already decided to block.
pub fn du_blocking(root: &Path, options: &DuOptions) -> Result<DuTotals> {
    super::walk::walk_blocking(root, options)
}
