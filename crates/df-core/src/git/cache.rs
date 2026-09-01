//! The status cache, and the one thread allowed to block on `git`.
//!
//! Everything the list pane asks of git goes through [`Git`], and every answer
//! is a hash lookup on data that is already in memory. Nothing here can block:
//! asking about a repository that has never been scanned queues a scan and
//! returns `None`, which the pane renders as "no dots yet" and repaints out of
//! when the notifier fires. That is the whole design, and it is the reason there
//! is no `status_now()` on this type.
//!
//! ## One worker
//!
//! `git status` on a large repository is seconds of work and mostly `stat`
//! calls, so two of them on one repository is slower than one. And the number of
//! repositories in play is the number of open tabs, which is small. One thread,
//! a request queue, and a dedup set that keeps a burst of `refresh()` calls —
//! one per inotify event during a `cargo build`, say — from queueing the same
//! repository forty times.
//!
//! ## Generations
//!
//! A repaint should not diff a hundred thousand paths to find out whether
//! anything changed. [`Git::generation`] is bumped once per stored status; df-app
//! keeps the last value it drew and compares. Each [`RepoStatus`] carries the
//! generation it was stored at, so a caller can also tell *which* repository
//! moved.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Sender};

use crate::fs::Notifier;

use super::repo::{self, Head};
use super::status::{self, DirtyCounts, FileStatus, StatusData, StatusError};

/// How many repositories keep a cached status.
///
/// One per open tab, plus the parent-pane repository, plus a little room for
/// tab-hopping. Eight is past that and keeps the worst case — eight
/// [`MAX_STATUS_ENTRIES`](super::status::MAX_STATUS_ENTRIES)-sized maps — from
/// being the reason the process is large. Eviction is oldest-scanned-first,
/// which for this access pattern is the tab you have not looked at in longest.
pub const GIT_REPOS: usize = 8;

/// How many repo-root lookups are memoized.
///
/// The walk-up is cheap but it happens for every directory entered and every
/// preview, and the answer never changes for a given path. 1024 covers a long
/// session's worth of directories at a few dozen bytes each; past it the map is
/// cleared wholesale rather than evicted one at a time, because the cost of
/// being wrong is one `stat` walk and the cost of a smarter policy is code.
pub const ROOT_MEMO: usize = 1024;

/// One repository's answer, as the cache holds it.
#[derive(Debug, Clone)]
pub struct RepoStatus {
    /// The work-tree root. Every path in [`RepoStatus::data`] is under it.
    pub root: PathBuf,
    /// What `HEAD` says, read from `.git` rather than from the porcelain, so it
    /// is right even when the status is stale.
    pub head: Option<Head>,
    pub data: StatusData,
    /// The value [`Git::generation`] had when this was stored.
    pub generation: u64,
}

impl RepoStatus {
    /// The dot for one row, or nothing.
    pub fn status_for(&self, path: &Path) -> Option<FileStatus> {
        self.data.status_for(path)
    }

    pub fn counts(&self) -> DirtyCounts {
        self.data.counts
    }

    /// The breadcrumb's branch text: the porcelain's spelling when there is one,
    /// otherwise `HEAD`'s — which is also the detached short hash.
    pub fn branch(&self) -> Option<&str> {
        self.data
            .branch
            .as_deref()
            .or_else(|| self.head.as_ref().map(|h| h.label()))
    }
}

#[derive(Default)]
struct Cache {
    repos: HashMap<PathBuf, Arc<RepoStatus>>,
    roots: HashMap<PathBuf, Option<PathBuf>>,
    pending: HashSet<PathBuf>,
}

struct State {
    cache: Mutex<Cache>,
    generation: AtomicU64,
    /// Latched false the first time a spawn says `NotFound`. Never latched back:
    /// git does not get installed mid-session, and if it does, restarting the
    /// file manager is a smaller surprise than a status that appears an hour in.
    available: AtomicBool,
}

/// The git front end. One per app.
///
/// Dropping it closes the request queue and joins the worker, so a test cannot
/// outlive its thread.
pub struct Git {
    state: Arc<State>,
    requests: Option<Sender<PathBuf>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Git {
    /// Start the worker. `notify` is rung once per stored status.
    pub fn start(notify: Notifier) -> Git {
        let state = Arc::new(State {
            cache: Mutex::new(Cache::default()),
            generation: AtomicU64::new(0),
            available: AtomicBool::new(true),
        });
        let (tx, rx) = unbounded::<PathBuf>();
        let worker_state = Arc::clone(&state);
        let handle = std::thread::Builder::new()
            .name("df-git".to_string())
            .spawn(move || {
                for root in rx {
                    run_one(&worker_state, &root, &notify);
                }
            });
        let worker = match handle {
            Ok(h) => Some(h),
            // No thread, no status. The rest of the file manager is unaffected,
            // which is the whole point of the feature being a decoration.
            Err(e) => {
                log::warn!("git worker did not start: {e}");
                state.available.store(false, Ordering::Relaxed);
                None
            }
        };
        Git {
            state,
            requests: Some(tx),
            worker,
        }
    }

    /// A cache that will never scan anything.
    ///
    /// For `git.enabled = false` in config, and for tests that want the lookup
    /// surface without a subprocess. Every accessor still works and every one
    /// returns nothing, so no caller needs an `if`.
    pub fn disabled() -> Git {
        let state = Arc::new(State {
            cache: Mutex::new(Cache::default()),
            generation: AtomicU64::new(0),
            available: AtomicBool::new(false),
        });
        Git {
            state,
            requests: None,
            worker: None,
        }
    }

    /// Whether git is still believed to be installed and enabled.
    pub fn available(&self) -> bool {
        self.state.available.load(Ordering::Relaxed)
    }

    /// Bumped once per stored status. A repaint that finds this unchanged has
    /// nothing to redraw.
    pub fn generation(&self) -> u64 {
        self.state.generation.load(Ordering::Relaxed)
    }

    /// The repository root containing `path`, memoized.
    ///
    /// `None` is cached too: most directories are not in a repository, and the
    /// walk-up for those is the one that costs the most (it goes all the way to
    /// `/`).
    pub fn repo_root(&self, path: &Path) -> Option<PathBuf> {
        if let Some(hit) = lock(&self.state.cache).roots.get(path) {
            return hit.clone();
        }
        let found = repo::repo_root(path);
        let mut cache = lock(&self.state.cache);
        if cache.roots.len() >= ROOT_MEMO {
            cache.roots.clear();
        }
        cache.roots.insert(path.to_path_buf(), found.clone());
        found
    }

    /// Whatever is currently known about `root`. Never blocks, never scans.
    pub fn status(&self, root: &Path) -> Option<Arc<RepoStatus>> {
        lock(&self.state.cache).repos.get(root).cloned()
    }

    /// The status for the repository containing `path`, queueing a first scan if
    /// there has never been one.
    ///
    /// This is the call the list pane makes when it enters a directory: it
    /// returns immediately with whatever exists, and the notifier brings the
    /// rest.
    pub fn ensure(&self, path: &Path) -> Option<Arc<RepoStatus>> {
        let root = self.repo_root(path)?;
        let known = self.status(&root);
        if known.is_none() {
            self.refresh(&root);
        }
        known
    }

    /// Queue a rescan of `root`.
    ///
    /// Idempotent while one is queued or running, so the burst of watch events a
    /// build produces costs one scan, not one per event. Debouncing *when* to
    /// call this is the caller's business — [`crate::fs::Watcher`] already
    /// coalesces, and the right delay depends on which directory is on screen.
    pub fn refresh(&self, root: &Path) {
        if !self.available() {
            return;
        }
        let Some(requests) = &self.requests else {
            return;
        };
        {
            let mut cache = lock(&self.state.cache);
            if !cache.pending.insert(root.to_path_buf()) {
                return;
            }
        }
        if requests.send(root.to_path_buf()).is_err() {
            lock(&self.state.cache).pending.remove(root);
        }
    }

    /// Rescan every repository already in the cache. For a window regaining
    /// focus, where anything could have happened in a terminal meanwhile.
    pub fn refresh_all(&self) {
        let roots: Vec<PathBuf> = lock(&self.state.cache).repos.keys().cloned().collect();
        for root in roots {
            self.refresh(&root);
        }
    }

    /// The dot for one row: finds the repository, then looks the path up.
    ///
    /// For a whole pane, prefer [`Git::ensure`] once and
    /// [`RepoStatus::status_for`] per row — this walks up for the root on every
    /// call, and a pane is a thousand rows.
    pub fn status_for(&self, path: &Path) -> Option<FileStatus> {
        let root = self.repo_root(path)?;
        self.status(&root)?.status_for(path)
    }

    /// The branch for the breadcrumb, without waiting for a status.
    ///
    /// Reads `.git/HEAD` directly on a cache miss, because it is a 41-byte read
    /// and the breadcrumb is drawn before the first scan finishes.
    pub fn branch(&self, root: &Path) -> Option<String> {
        if let Some(status) = self.status(root) {
            if let Some(name) = status.branch() {
                return Some(name.to_string());
            }
        }
        repo::branch(root)
    }

    /// The dirty count beside the branch, or nothing if no scan has landed.
    pub fn counts(&self, root: &Path) -> Option<DirtyCounts> {
        self.status(root).map(|s| s.counts())
    }

    /// Drop one repository's cached status — a tab closing, or a repository the
    /// user just deleted.
    pub fn forget(&self, root: &Path) {
        let mut cache = lock(&self.state.cache);
        cache.repos.remove(root);
        cache.roots.retain(|_, v| v.as_deref() != Some(root));
    }

    /// Drop everything, including the root memo. For a config reload.
    pub fn clear(&self) {
        let mut cache = lock(&self.state.cache);
        cache.repos.clear();
        cache.roots.clear();
    }

    /// Whether a scan is queued or running for `root`. Tests wait on this; the
    /// app has the notifier and does not need it.
    pub fn is_pending(&self, root: &Path) -> bool {
        lock(&self.state.cache).pending.contains(root)
    }
}

impl Drop for Git {
    fn drop(&mut self) {
        self.requests = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Lock without panicking on poison — the same argument as
/// [`crate::fs::scan`]'s: a poisoned status cache is a stale status cache, and
/// killing the file manager over one is strictly worse than showing last
/// second's dots.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn run_one(state: &State, root: &Path, notify: &Notifier) {
    let result = status::status_blocking(root);
    let mut cache = lock(&state.cache);
    cache.pending.remove(root);

    let data = match result {
        Ok(data) => data,
        Err(StatusError::NoGit) => {
            // Once. Every subsequent `refresh` returns before spawning.
            state.available.store(false, Ordering::Relaxed);
            log::info!("git is not installed; status dots are off");
            return;
        }
        Err(e) => {
            // A directory that stopped being a repository, a corrupt index, a
            // permission problem. Nothing the user can act on from a file
            // manager, so it stays in the log and the rows keep their last dots.
            log::debug!("git status in {}: {e}", root.display());
            return;
        }
    };

    let generation = state.generation.fetch_add(1, Ordering::Relaxed) + 1;
    let status = Arc::new(RepoStatus {
        root: root.to_path_buf(),
        head: repo::head(root),
        data,
        generation,
    });

    if cache.repos.len() >= GIT_REPOS && !cache.repos.contains_key(root) {
        // Oldest scan wins the eviction. `generation` is monotonic, so the
        // smallest one is the least recently refreshed.
        if let Some(oldest) = cache
            .repos
            .iter()
            .min_by_key(|(_, s)| s.generation)
            .map(|(k, _)| k.clone())
        {
            cache.repos.remove(&oldest);
        }
    }
    cache.repos.insert(root.to_path_buf(), status);
    drop(cache);
    notify();
}
