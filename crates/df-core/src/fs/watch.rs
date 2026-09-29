//! Watching the directories on screen, so the list is never stale.
//!
//! Untar an archive in a terminal and the pane showing that directory has to
//! fill in as the files land — without a poll loop, and without a rescan per
//! file. That is this module: a watch on the directories currently displayed
//! (the list and its parent, PLAN §2), a burst debounce, and a refresh event
//! per affected directory through the same [`Notifier`] the scanner uses. The
//! watching itself is the platform's ([`crate::platform::watch`]): inotify on
//! Linux, and nothing yet elsewhere.
//!
//! **Watching is an enhancement, never a dependency.** `inotify_init1` can fail
//! (`/proc/sys/fs/inotify/max_user_instances` is 128 by default and a browser
//! or an editor will happily eat most of it) and `add_watch` can fail per
//! directory (`max_user_watches`, or a path that stopped being a directory).
//! Every one of those is logged and shrugged off: the model still loads, sorts
//! and navigates, it just does not notice a change it was not told about. A
//! file manager that refused to start because it ran out of watches would be a
//! worse program than one that occasionally needs a manual reload. A platform
//! with no watcher at all is the same case, from the first call.
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

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};

use super::scan::Notifier;
use crate::platform::watch::Backend;

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

/// Messages to the watcher thread, which is the platform's
/// ([`crate::platform::watch`]).
pub(crate) enum Control {
    /// Replace the whole watched set. Replace rather than add/remove because
    /// the caller's truth is "these are the directories on screen", and
    /// diffing that against the kernel's set is the watcher's job, not the
    /// caller's. Only a platform's watcher thread reads the list, and a
    /// platform without one (its backend never opens) never does.
    #[allow(dead_code)]
    Watch(Vec<PathBuf>),
    Stop,
}

/// A running watcher, or nothing at all.
///
/// Construction is fallible; the caller is expected to log and continue. See
/// [`Watcher::disabled`] for the "carry on without it" shape.
pub struct Watcher {
    control: Sender<Control>,
    events: Receiver<WatchEvent>,
    /// The platform's half, which can interrupt the thread's wait when a
    /// control message is queued. `None` on a disabled watcher.
    backend: Option<Backend>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    /// Start watching. `notify` is rung whenever events become available, the
    /// same bell the scanner rings.
    pub fn new(notify: Notifier) -> io::Result<Watcher> {
        let (ctl_tx, ctl_rx) = unbounded::<Control>();
        let (ev_tx, ev_rx) = unbounded::<WatchEvent>();
        let (backend, thread) = Backend::open(ctl_rx, ev_tx, notify)?;

        Ok(Watcher {
            control: ctl_tx,
            events: ev_rx,
            backend: Some(backend),
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
                log::warn!(
                    "directory watching unavailable ({e}); directories will not auto-refresh"
                );
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
            backend: None,
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
        if let Some(backend) = &self.backend {
            backend.wake();
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
