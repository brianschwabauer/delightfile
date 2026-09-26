//! Reading a directory off the event loop, in batches, cancellably.
//!
//! Two facts set the shape of this file. A directory read can block for a long
//! time — a spinning disk waking up, an NFS mount that has gone away, 200k
//! entries each needing a `statx` — and the event loop must never be the thread
//! it blocks on (PLAN §1). And the first paint has to be *immediate*: yazi
//! feels instant because it shows you rows before it has finished counting
//! them, and anything slower than that is a regression Brian would feel on the
//! first keypress.
//!
//! So: worker threads read, results arrive in batches on a crossbeam channel,
//! and the app thread drains it when something rings the bell. df-core stays
//! runtime-agnostic (PLAN §1) — it knows nothing about winit — so "ring the
//! bell" is a [`Notifier`] callback the app supplies, and the app's version of
//! it sends its `Wake` user event. No polling, anywhere.
//!
//! ## Staleness
//!
//! Arrow down five directories in half a second and five scans are in flight,
//! four of which are answers to questions nobody is asking any more. Every scan
//! gets a monotonic [`ScanToken`], and every update carries the token it
//! belongs to; a model drops updates whose token is not the one it asked for.
//! That is the *display* half. The *work* half is [`Scanner::cancel`]: the
//! worker checks its token against the live set between batches and stops
//! walking, so a cancelled scan of a huge tree stops costing syscalls rather
//! than merely stopping being drawn.
//!
//! Starting a scan of a directory automatically cancels any in-flight scan of
//! that same directory, which is what a rescan-on-file-change is. Scans of
//! *different* directories run side by side untouched, because the parent pane,
//! the list and a directory preview are three concurrent scans and none of them
//! is stale (PLAN §2).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};

use super::entry::Entry;
use crate::{DfError, Result};

/// How many entries the first batch carries.
///
/// The number that matters is "one screenful", because the only thing the first
/// batch has to do is let the list paint something true. 64 rows is a tall pane
/// on a 4K display at delightfile's row height with room to spare; it also
/// bounds the work between opening the directory and the first repaint to 64
/// `statx` calls, a fraction of a millisecond even cold.
pub const FIRST_BATCH: usize = 64;

/// Entries per batch after the first.
///
/// Every batch costs a channel send, a wake-up and a repaint, so past the first
/// screen the goal flips from latency to throughput: 512 entries is ~8 batches
/// for a 4k-file directory and ~400 for a pathological 200k one, which keeps
/// the UI thread's share of a big scan to a few hundred cheap wake-ups instead
/// of a few hundred thousand.
pub const BATCH: usize = 512;

/// Scan workers.
///
/// Three, because three directories are legitimately being read at once: the
/// parent pane, the list, and a directory under the cursor being previewed
/// (PLAN §2). A fourth would only ever be servicing a scan that is already
/// stale. Independent of the task pool's 10/10 (PLAN §5) — those workers move
/// bytes, these only ever read metadata.
pub const SCAN_WORKERS: usize = 3;

/// Identifies one scan. Monotonic, per [`Scanner`], never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScanToken(pub u64);

/// What a worker sends back.
#[derive(Debug)]
pub enum ScanUpdate {
    /// The directory opened. Sent before any entries, so the model can clear
    /// the old listing at the moment it knows a new one is coming rather than
    /// blanking the pane the instant a key was pressed.
    Started { token: ScanToken, dir: PathBuf },
    /// Some entries, in `read_dir` order (sorting is the model's job — see
    /// [`super::sort_order`]).
    Batch {
        token: ScanToken,
        dir: PathBuf,
        entries: Vec<Entry>,
    },
    /// Every entry has been sent.
    Done {
        token: ScanToken,
        dir: PathBuf,
        total: usize,
    },
    /// The directory could not be read — no permission, gone, not a directory.
    /// A failure the user gets told about, which is why it carries the path.
    Failed {
        token: ScanToken,
        dir: PathBuf,
        error: DfError,
    },
}

impl ScanUpdate {
    pub fn token(&self) -> ScanToken {
        match self {
            ScanUpdate::Started { token, .. }
            | ScanUpdate::Batch { token, .. }
            | ScanUpdate::Done { token, .. }
            | ScanUpdate::Failed { token, .. } => *token,
        }
    }

    pub fn dir(&self) -> &Path {
        match self {
            ScanUpdate::Started { dir, .. }
            | ScanUpdate::Batch { dir, .. }
            | ScanUpdate::Done { dir, .. }
            | ScanUpdate::Failed { dir, .. } => dir,
        }
    }
}

/// The app's "something arrived, wake up" hook.
///
/// A callback rather than a channel the app selects on, because df-core must
/// not know what the app waits on (PLAN §1): df-app hands over a closure that
/// posts its `Wake` user event, and a test hands over one that bumps a counter.
pub type Notifier = Arc<dyn Fn() + Send + Sync>;

/// A do-nothing notifier, for callers that drain the channel themselves.
pub fn no_notifier() -> Notifier {
    Arc::new(|| {})
}

struct Request {
    token: ScanToken,
    dir: PathBuf,
}

/// Shared between the scanner and its workers: which tokens are still wanted.
///
/// A map rather than a set so that starting a scan can cancel the previous scan
/// *of the same directory* without the caller tracking tokens.
type Live = Arc<Mutex<HashMap<ScanToken, PathBuf>>>;

/// The worker pool.
///
/// Dropping it closes the request channel; workers finish the batch they are on
/// and exit, and the drop joins them so a test cannot outlive its threads.
pub struct Scanner {
    requests: Option<Sender<Request>>,
    updates: Receiver<ScanUpdate>,
    live: Live,
    next_token: AtomicU64,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Scanner {
    /// Start `workers` threads. `notify` is rung once per update sent.
    pub fn new(workers: usize, notify: Notifier) -> Scanner {
        let (req_tx, req_rx) = unbounded::<Request>();
        let (up_tx, up_rx) = unbounded::<ScanUpdate>();
        let live: Live = Arc::new(Mutex::new(HashMap::new()));

        let mut handles = Vec::with_capacity(workers);
        for i in 0..workers.max(1) {
            let req_rx = req_rx.clone();
            let up_tx = up_tx.clone();
            let live = Arc::clone(&live);
            let notify = Arc::clone(&notify);
            let handle = std::thread::Builder::new()
                .name(format!("df-scan-{i}"))
                .spawn(move || {
                    for request in req_rx {
                        run_scan(request, &up_tx, &live, &notify);
                    }
                });
            match handle {
                Ok(h) => handles.push(h),
                // A thread that will not spawn is not fatal: the remaining
                // workers (or, at worst, none — see `scan_blocking`) still
                // answer. Better a slow file manager than no file manager.
                Err(e) => log::warn!("scan worker {i} did not start: {e}"),
            }
        }

        Scanner {
            requests: Some(req_tx),
            updates: up_rx,
            live,
            next_token: AtomicU64::new(1),
            workers: handles,
        }
    }

    /// A scanner with the default worker count.
    pub fn start(notify: Notifier) -> Scanner {
        Scanner::new(SCAN_WORKERS, notify)
    }

    /// Queue a scan and return its token. Any in-flight scan of the same
    /// directory is cancelled first.
    pub fn scan(&self, dir: impl Into<PathBuf>) -> ScanToken {
        let dir = dir.into();
        let token = ScanToken(self.next_token.fetch_add(1, Ordering::Relaxed));
        {
            let mut live = lock(&self.live);
            live.retain(|_, d| *d != dir);
            live.insert(token, dir.clone());
        }
        if let Some(requests) = &self.requests {
            // The channel is unbounded and the receivers are only dropped when
            // the whole scanner is, so a send failure means the pool is gone.
            if requests.send(Request { token, dir }).is_err() {
                lock(&self.live).remove(&token);
            }
        }
        token
    }

    /// Stop a scan. Already-sent batches may still be in the channel — the
    /// model discards them by token — but no further filesystem work is done.
    pub fn cancel(&self, token: ScanToken) {
        lock(&self.live).remove(&token);
    }

    /// Stop everything in flight (leaving a tab, quitting).
    pub fn cancel_all(&self) {
        lock(&self.live).clear();
    }

    /// Whether this scan is still wanted.
    pub fn is_live(&self, token: ScanToken) -> bool {
        lock(&self.live).contains_key(&token)
    }

    /// The channel, for a caller that wants to select on it.
    pub fn updates(&self) -> &Receiver<ScanUpdate> {
        &self.updates
    }

    /// Everything that has arrived, without blocking. This is what the app
    /// calls when its `Wake` event fires.
    pub fn drain(&self) -> Vec<ScanUpdate> {
        self.updates.try_iter().collect()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.cancel_all();
        // Closing the request channel is what tells the workers to exit; they
        // check `live` between batches, so a scan in progress stops promptly.
        self.requests = None;
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

/// Lock without ever panicking on a poisoned mutex.
///
/// Poisoning means some other thread panicked while holding this lock. The data
/// behind it is a set of `u64`s — there is no invariant a panic could have left
/// half-applied — so the correct response is to carry on rather than to take
/// the whole file manager down. `unwrap_used` is a workspace lint for exactly
/// this reason: every unwrap has to have an answer, and this one's answer is
/// "there is nothing to recover".
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn run_scan(request: Request, updates: &Sender<ScanUpdate>, live: &Live, notify: &Notifier) {
    let Request { token, dir } = request;
    // Cancelled before a worker even picked it up — the common case when
    // arrowing quickly through directories.
    if !lock(live).contains_key(&token) {
        return;
    }

    let send = |update: ScanUpdate| -> bool {
        let ok = updates.send(update).is_ok();
        if ok {
            notify();
        }
        ok
    };

    let reader = match std::fs::read_dir(&dir) {
        Ok(reader) => reader,
        Err(e) => {
            lock(live).remove(&token);
            send(ScanUpdate::Failed {
                token,
                dir: dir.clone(),
                error: DfError::io(dir, e),
            });
            return;
        }
    };

    if !send(ScanUpdate::Started {
        token,
        dir: dir.clone(),
    }) {
        return;
    }

    let mut batch: Vec<Entry> = Vec::with_capacity(FIRST_BATCH);
    let mut limit = FIRST_BATCH;
    let mut total = 0usize;
    // One `statfs` for the whole directory: whether its rows' tags are read
    // at all, which on a network or FUSE mount they are not.
    let tags = super::tags::read_here(&dir);

    for item in reader {
        // Checked per entry rather than per batch: a directory of 200k entries
        // on a cold mount can spend seconds inside one batch, and the point of
        // cancelling is to stop *now*.
        if !lock(live).contains_key(&token) {
            return;
        }
        let Ok(item) = item else {
            // One unreadable entry does not fail the directory. It cannot be
            // listed, so it is skipped — the alternative is a pane that shows
            // nothing because one file went away mid-scan.
            continue;
        };
        let Ok(meta) = item.metadata() else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        batch.push(Entry::from_parts(name, item.path(), meta, tags));
        total += 1;

        if batch.len() >= limit {
            limit = BATCH;
            let entries = std::mem::replace(&mut batch, Vec::with_capacity(BATCH));
            if !send(ScanUpdate::Batch {
                token,
                dir: dir.clone(),
                entries,
            }) {
                return;
            }
        }
    }

    if !batch.is_empty()
        && !send(ScanUpdate::Batch {
            token,
            dir: dir.clone(),
            entries: batch,
        })
    {
        return;
    }

    lock(live).remove(&token);
    send(ScanUpdate::Done { token, dir, total });
}

/// Read a whole directory on the calling thread.
///
/// For the places where blocking is the right answer anyway: tests, and the
/// synchronous parent-directory read at startup where there is nothing to paint
/// yet. Entries come back in `read_dir` order, unsorted.
pub fn scan_blocking(dir: &Path) -> Result<Vec<Entry>> {
    let reader = std::fs::read_dir(dir).map_err(|e| DfError::io(dir, e))?;
    let tags = super::tags::read_here(dir);
    let mut entries = Vec::new();
    for item in reader.flatten() {
        let Ok(meta) = item.metadata() else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        entries.push(Entry::from_parts(name, item.path(), meta, tags));
    }
    Ok(entries)
}
