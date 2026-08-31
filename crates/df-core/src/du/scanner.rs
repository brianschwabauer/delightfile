//! The worker pool that runs walks in the background.
//!
//! Structurally this is [`crate::fs::scan::Scanner`]'s twin, and deliberately
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
//! see [`super::walk`], which does all three.
//!
//! **The answer is worth keeping.** A directory scan is cheap enough to redo;
//! a du is not, so every completed walk lands in a [`DuCache`] the scanner owns
//! and [`DuScanner::heavy_hitters`] reads. That cache is the "what's big" mode's
//! actual data structure — the message stream is only how it fills up live.
//!
//! **Two workers, not three.** These threads are not waiting on a slow mount;
//! they are issuing `stat` back to back, and past two of them a du of one tree
//! is competing with itself for the same page cache and the same disk queue.
//! Two covers the real concurrency: the tree you are looking at, and the one you
//! just left that has not noticed yet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::fs::Notifier;
use crate::{DfError, Result};

use super::cache::{current_mtime, DuCache, DuRecord, HeavyHitter};
use super::walk::{walk, DuOptions, DuTotals, DuUpdate};

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
            | DuMessage::Progress { token, .. }
            | DuMessage::Done { token, .. }
            | DuMessage::Failed { token, .. } => *token,
        }
    }

    pub fn root(&self) -> &Path {
        match self {
            DuMessage::Started { root, .. }
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

type Live = Arc<Mutex<HashMap<DuToken, PathBuf>>>;
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
            live.retain(|_, r| *r != root);
            live.insert(token, root.clone());
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
                lock(&self.live).remove(&token);
            }
        }
        token
    }

    /// Stop a walk. Messages already in the channel may still arrive; the
    /// consumer drops them by token.
    pub fn cancel(&self, token: DuToken) {
        lock(&self.live).remove(&token);
    }

    /// Stop everything in flight (leaving the mode, closing the tab, quitting).
    pub fn cancel_all(&self) {
        lock(&self.live).clear();
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

    /// A directory's remembered totals, if fresh. See [`DuCache::get`] — and
    /// its warning that "fresh" means the directory's own `mtime`, not its
    /// subtree's.
    pub fn cached(&self, dir: &Path) -> Option<DuRecord> {
        lock(&self.cache).get(dir).cloned()
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
        lock(&self.cache).heavy_hitters(dir)
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
        let Some(record) = cache.get(dir) else {
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
    if !lock(live).contains_key(&token) {
        return;
    }

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

    // Read before the walk, not after: a directory changed *during* the walk
    // must invalidate the record, and stamping it with the mtime we saw first
    // is what makes that happen.
    let root_mtime = current_mtime(&root);

    let mut tracked: HashMap<PathBuf, DuTotals> = HashMap::new();
    let mut tracking_complete = true;
    let mut channel_open = true;

    {
        let cancelled = || !lock(live).contains_key(&token);
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
                tracked.insert(update.dir.clone(), update.totals());
            }
            if channel_open {
                channel_open = send(DuMessage::Progress {
                    token,
                    root: root.clone(),
                    updates: batch,
                });
            }
        };

        match walk(&root, &options, &cancelled, &mut emit) {
            Ok(totals) => {
                lock(live).remove(&token);
                store(cache, &root, root_mtime, totals, &tracked, tracking_complete);
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
                lock(live).remove(&token);
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
/// Only directories are children here. A big *file* is a heavy hitter too, but
/// its size is already in the listing (`Entry::len` is honest for files), so
/// making the walk carry file names as well would double the memory to
/// re-derive something the caller has.
fn store(
    cache: &Shared,
    root: &Path,
    root_mtime: Option<std::time::SystemTime>,
    totals: DuTotals,
    tracked: &HashMap<PathBuf, DuTotals>,
    complete: bool,
) {
    let mut children: HashMap<&Path, Vec<(String, DuTotals)>> = HashMap::new();
    for (dir, dir_totals) in tracked {
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
    for (dir, dir_totals) in tracked {
        let kids = children.remove(dir.as_path()).unwrap_or_default();
        let mtime = if dir == root {
            root_mtime
        } else {
            current_mtime(dir)
        };
        cache.insert(dir, mtime, *dir_totals, kids, complete);
    }
    // A `depth_of_interest` of 0 emits nothing for the root's children, and a
    // root with no subdirectories tracks only itself; either way the root's own
    // totals must still land.
    if !tracked.contains_key(root) {
        cache.insert(root, root_mtime, totals, Vec::new(), complete);
    }
}

/// Walk one tree on the calling thread, ignoring the stream.
///
/// The synchronous door into the same machinery, for tests and for a caller that
/// has already decided to block.
pub fn du_blocking(root: &Path, options: &DuOptions) -> Result<DuTotals> {
    super::walk::walk_blocking(root, options)
}
