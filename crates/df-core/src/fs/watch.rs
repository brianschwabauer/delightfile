//! Watching the directories on screen, so the list is never stale.
//!
//! Untar an archive in a terminal and the pane showing that directory has to
//! fill in as the files land — without a poll loop, and without a rescan per
//! file. That is this module: an inotify watch on the directories currently
//! displayed (the list and its parent, PLAN §2), a burst debounce, and a
//! refresh event per affected directory through the same [`Notifier`] the
//! scanner uses.
//!
//! **Watching is an enhancement, never a dependency.** `inotify_init1` can fail
//! (`/proc/sys/fs/inotify/max_user_instances` is 128 by default and a browser
//! or an editor will happily eat most of it) and `add_watch` can fail per
//! directory (`max_user_watches`, or a path that stopped being a directory).
//! Every one of those is logged and shrugged off: the model still loads, sorts
//! and navigates, it just does not notice a change it was not told about. A
//! file manager that refused to start because it ran out of watches would be a
//! worse program than one that occasionally needs a manual reload.
//!
//! ## Debounce
//!
//! One `git checkout` is thousands of events in a few milliseconds, and one
//! rescan is the correct response to all of them. Dirty directories are
//! collected and flushed [`DEBOUNCE`] after the *first* event of a burst — a
//! deadline, not a sliding window, because a sliding one starves: a directory
//! being written to continuously would never flush, and "the pane updates when
//! the copy finishes" is precisely the behaviour that makes a file manager feel
//! dead.
//!
//! The watcher thread sleeps in `poll` between bursts (see [`super::inotify`]),
//! so an idle delightfile costs zero wake-ups — PLAN §1's idle-cost constraint
//! applies to worker threads too.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use super::inotify::{self, Inotify, Pipe, WATCH_MASK};
use super::scan::Notifier;

/// How long a burst is allowed to accumulate before the pane is refreshed.
///
/// 80 ms is the point where a change still reads as a direct consequence of
/// what just happened (under ~100 ms feels like "it did that", above ~150 ms
/// like "then it updated"), while being long enough to swallow the thousands of
/// events an unpack or a `git checkout` emits into a single rescan. It is also
/// comfortably under the ~120 ms of the row fade in PLAN §8, so the refresh
/// arrives before the previous animation has finished and the two do not fight.
pub const DEBOUNCE: Duration = Duration::from_millis(80);

/// What the watcher tells the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// Something in this directory changed; rescan it.
    Changed(PathBuf),
    /// The directory being watched was itself deleted or moved away. The pane
    /// showing it has to leave rather than reload.
    Gone(PathBuf),
    /// The kernel queue overflowed and events were lost. Nothing is known about
    /// what changed, so **everything** on screen must be rescanned — this is
    /// the one event that is not about a single directory.
    Overflow,
}

/// Messages to the watcher thread.
enum Control {
    /// Replace the whole watched set. Replace rather than add/remove because
    /// the caller's truth is "these are the directories on screen", and
    /// diffing that against the kernel's set is this module's job, not the
    /// caller's.
    Watch(Vec<PathBuf>),
    Stop,
}

/// A running inotify watcher, or nothing at all.
///
/// Construction is fallible; the caller is expected to log and continue. See
/// [`Watcher::disabled`] for the "carry on without it" shape.
pub struct Watcher {
    control: Sender<Control>,
    events: Receiver<WatchEvent>,
    /// Shared with the thread so a control message can interrupt its `poll`.
    /// `None` on a disabled watcher.
    pipe: Option<Arc<Pipe>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    /// Start watching. `notify` is rung whenever events become available, the
    /// same bell the scanner rings.
    pub fn new(notify: Notifier) -> io::Result<Watcher> {
        let inotify = Inotify::new()?;
        let pipe = Arc::new(Pipe::new()?);
        let (ctl_tx, ctl_rx) = unbounded::<Control>();
        let (ev_tx, ev_rx) = unbounded::<WatchEvent>();

        let thread_pipe = Arc::clone(&pipe);
        let thread = std::thread::Builder::new()
            .name("df-watch".to_string())
            .spawn(move || run(inotify, thread_pipe, ctl_rx, ev_tx, notify))?;

        Ok(Watcher {
            control: ctl_tx,
            events: ev_rx,
            pipe: Some(pipe),
            thread: Some(thread),
        })
    }

    /// Start watching, or return a watcher that does nothing.
    ///
    /// The shape the app uses: there is no branch at the call site, because
    /// "we could not watch" changes nothing about how the model is driven —
    /// events simply never arrive.
    pub fn start(notify: Notifier) -> Watcher {
        match Watcher::new(notify) {
            Ok(w) => w,
            Err(e) => {
                log::warn!("inotify unavailable ({e}); directories will not auto-refresh");
                Watcher::disabled()
            }
        }
    }

    /// A watcher that watches nothing and emits nothing.
    pub fn disabled() -> Watcher {
        let (control, _) = unbounded::<Control>();
        let (_, events) = unbounded::<WatchEvent>();
        Watcher {
            control,
            events,
            pipe: None,
            thread: None,
        }
    }

    /// Whether this watcher is actually watching.
    pub fn is_active(&self) -> bool {
        self.thread.is_some()
    }

    /// Set the watched directories to exactly these — typically the list's
    /// directory and its parent (PLAN §2). Called on every navigation.
    pub fn watch(&self, dirs: Vec<PathBuf>) {
        if self.control.send(Control::Watch(dirs)).is_ok() {
            self.interrupt();
        }
    }

    /// The channel, for a caller that wants to select on it.
    pub fn events(&self) -> &Receiver<WatchEvent> {
        &self.events
    }

    /// Everything that has arrived, without blocking.
    pub fn drain(&self) -> Vec<WatchEvent> {
        self.events.try_iter().collect()
    }

    fn interrupt(&self) {
        if let Some(pipe) = &self.pipe {
            pipe.wake();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Stop);
        self.interrupt();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The watcher thread.
fn run(
    inotify: Inotify,
    pipe: Arc<Pipe>,
    control: Receiver<Control>,
    events: Sender<WatchEvent>,
    notify: Notifier,
) {
    // wd → the directory it belongs to. The kernel gives back the *same* wd for
    // a directory already being watched, so this doubles as the "already
    // watching it" check.
    let mut watched: HashMap<i32, PathBuf> = HashMap::new();
    let mut dirty: Vec<PathBuf> = Vec::new();
    let mut gone: Vec<PathBuf> = Vec::new();
    let mut overflow = false;
    let mut deadline: Option<Instant> = None;

    loop {
        let timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        let ready = inotify::poll_two(inotify.fd(), pipe.read_fd(), timeout);
        let (inotify_ready, pipe_ready) = match ready {
            Ok(r) => r,
            Err(e) => {
                log::warn!("inotify poll failed ({e}); giving up on auto-refresh");
                return;
            }
        };

        if pipe_ready {
            pipe.drain();
            for message in control.try_iter() {
                match message {
                    Control::Watch(dirs) => set_watches(&inotify, &mut watched, dirs),
                    Control::Stop => return,
                }
            }
        }

        if inotify_ready {
            match inotify.read_events() {
                Ok(list) => {
                    for event in list {
                        if event.is_overflow() {
                            overflow = true;
                            continue;
                        }
                        let Some(dir) = watched.get(&event.wd).cloned() else {
                            continue;
                        };
                        if event.is_self_gone() {
                            // The kernel has already dropped this watch.
                            watched.remove(&event.wd);
                            if !gone.contains(&dir) {
                                gone.push(dir);
                            }
                            continue;
                        }
                        if !dirty.contains(&dir) {
                            dirty.push(dir);
                        }
                    }
                }
                Err(e) => log::warn!("inotify read failed: {e}"),
            }
            if (!dirty.is_empty() || !gone.is_empty() || overflow) && deadline.is_none() {
                deadline = Some(Instant::now() + DEBOUNCE);
            }
        }

        // The deadline, not a sliding window: a directory under continuous
        // write still refreshes every DEBOUNCE rather than never.
        if deadline.is_some_and(|d| Instant::now() >= d) {
            deadline = None;
            let mut sent = false;
            if overflow {
                overflow = false;
                // Overflow supersedes the per-directory list: nothing is known
                // about what changed, and the model rescans everything anyway.
                dirty.clear();
                sent |= events.send(WatchEvent::Overflow).is_ok();
            }
            for dir in dirty.drain(..) {
                sent |= events.send(WatchEvent::Changed(dir)).is_ok();
            }
            for dir in gone.drain(..) {
                sent |= events.send(WatchEvent::Gone(dir)).is_ok();
            }
            if sent {
                notify();
            } else {
                // Nobody is listening any more; the Watcher was dropped without
                // its Stop arriving.
                return;
            }
        }
    }
}

/// Make the kernel's watch set match `dirs` exactly.
fn set_watches(inotify: &Inotify, watched: &mut HashMap<i32, PathBuf>, dirs: Vec<PathBuf>) {
    let mut next: HashMap<i32, PathBuf> = HashMap::new();
    for dir in dirs {
        // `inotify_add_watch` on an already-watched path returns the existing
        // wd and refreshes the mask, so re-adding is free and there is no
        // "already watching?" branch to get wrong.
        match inotify.add_watch(&dir, WATCH_MASK) {
            Ok(wd) => {
                next.insert(wd, dir);
            }
            // Out of watches, or it stopped being a directory between the
            // navigation and this call. Either way: no auto-refresh for that
            // pane, and nothing else changes.
            Err(e) => log::debug!("cannot watch {}: {e}", dir.display()),
        }
    }
    for wd in watched.keys() {
        if !next.contains_key(wd) {
            inotify.rm_watch(*wd);
        }
    }
    *watched = next;
}
