//! The macOS watcher: a kqueue with one `EVFILT_VNODE` watch per directory on
//! screen, a self-pipe to wake the thread, and the burst debounce (see
//! [`crate::fs::Watcher`] for what the model does with the events).
//!
//! The thread sleeps in `kevent` between bursts, so an idle delightfile costs
//! no wake-ups, as on Linux. The events are the Linux watcher's, reached a
//! different way: a write to a watched directory — an entry made, removed or
//! renamed in it — is `Changed(dir)`, and the directory deleted, renamed away
//! or unmounted is `Gone(dir)`, after which its descriptor is closed. There is
//! no `Overflow`: a kqueue with `EV_CLEAR` folds any number of writes into one
//! event and never drops one.
//!
//! What it does not see, where inotify does: a file inside a watched directory
//! rewritten in place, or its mode changed. A directory's vnode hears only
//! about its own list of names, so a row's size or date is brought up to date
//! by the next rescan rather than by the write. Saving through a temporary
//! file and a rename — what most editors and every copy here do — does touch
//! the directory, and is seen.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

use super::kqueue::{Kqueue, Pipe, WatchedDir};
use crate::fs::watch::{Control, DEBOUNCE};
use crate::fs::{Notifier, WatchEvent};

/// What a running [`crate::fs::Watcher`] holds of the backend: the pipe that
/// interrupts the thread's `kevent` when a control message is waiting.
pub(crate) struct Backend {
    pipe: Arc<Pipe>,
}

impl Backend {
    /// Open a kqueue and the wake-up pipe and start the `df-watch` thread. The
    /// thread reads `control` and sends on `events`, ringing `notify` after
    /// each flushed burst. Fails when either cannot be had (the descriptor
    /// limit), which the caller survives.
    pub(crate) fn open(
        control: Receiver<Control>,
        events: Sender<WatchEvent>,
        notify: Notifier,
    ) -> io::Result<(Backend, std::thread::JoinHandle<()>)> {
        let kqueue = Kqueue::new()?;
        let pipe = Arc::new(Pipe::new()?);
        kqueue.watch_read(pipe.read_fd())?;

        let thread_pipe = Arc::clone(&pipe);
        let thread = std::thread::Builder::new()
            .name("df-watch".to_string())
            .spawn(move || run(kqueue, thread_pipe, control, events, notify))?;

        Ok((Backend { pipe }, thread))
    }

    /// Interrupt the thread's `kevent` so it reads the control channel.
    pub(crate) fn wake(&self) {
        self.pipe.wake();
    }
}

/// The watcher thread.
fn run(
    kqueue: Kqueue,
    pipe: Arc<Pipe>,
    control: Receiver<Control>,
    events: Sender<WatchEvent>,
    notify: Notifier,
) {
    // Descriptor → the directory it is open on. Dropping an entry closes the
    // descriptor, which is also what takes its watch off the kqueue.
    let mut watched: HashMap<i32, (WatchedDir, PathBuf)> = HashMap::new();
    let mut dirty: Vec<PathBuf> = Vec::new();
    let mut gone: Vec<PathBuf> = Vec::new();
    let mut deadline: Option<Instant> = None;

    loop {
        let timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        let list = match kqueue.wait(timeout) {
            Ok(list) => list,
            Err(e) => {
                log::warn!("kqueue wait failed ({e}); giving up on auto-refresh");
                return;
            }
        };

        // The directories' events first, while every descriptor in this batch
        // still means what it meant when the kernel queued it: a new watch set
        // below may close one and open another under the same number.
        let mut woken = false;
        for event in list {
            if event.filter == libc::EVFILT_READ {
                woken = true;
                continue;
            }
            let Ok(fd) = i32::try_from(event.ident) else {
                continue;
            };
            if event.is_gone() {
                // Closed here: a directory that is gone has nothing more to say.
                if let Some((_, dir)) = watched.remove(&fd) {
                    if !gone.contains(&dir) {
                        gone.push(dir);
                    }
                }
                continue;
            }
            if let Some((_, dir)) = watched.get(&fd) {
                if !dirty.contains(dir) {
                    dirty.push(dir.clone());
                }
            }
        }

        if woken {
            pipe.drain();
            for message in control.try_iter() {
                match message {
                    Control::Watch(dirs) => set_watches(&kqueue, &mut watched, dirs),
                    Control::Stop => return,
                }
            }
        }

        if (!dirty.is_empty() || !gone.is_empty()) && deadline.is_none() {
            deadline = Some(Instant::now() + DEBOUNCE);
        }

        // The deadline, not a sliding window: a directory under continuous
        // write still refreshes every DEBOUNCE rather than never.
        if deadline.is_some_and(|d| Instant::now() >= d) {
            deadline = None;
            let mut sent = false;
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

/// Make the watch set match `dirs` exactly: a directory already watched keeps
/// its descriptor, a new one is opened and registered, and one no longer
/// asked for is closed.
fn set_watches(
    kqueue: &Kqueue,
    watched: &mut HashMap<i32, (WatchedDir, PathBuf)>,
    dirs: Vec<PathBuf>,
) {
    let mut next: HashMap<i32, (WatchedDir, PathBuf)> = HashMap::new();
    for dir in dirs {
        if next.values().any(|(_, known)| *known == dir) {
            continue;
        }
        let kept = watched
            .iter()
            .find(|(_, (_, known))| *known == dir)
            .map(|(fd, _)| *fd);
        if let Some(entry) = kept.and_then(|fd| watched.remove(&fd)) {
            next.insert(entry.0.fd(), entry);
            continue;
        }
        let opened = WatchedDir::open(&dir).and_then(|handle| {
            kqueue.watch_dir(&handle)?;
            Ok(handle)
        });
        match opened {
            Ok(handle) => {
                next.insert(handle.fd(), (handle, dir));
            }
            // Out of descriptors, or it stopped being a directory between the
            // navigation and this call. Either way: no auto-refresh for that
            // pane, and nothing else changes.
            Err(e) => log::debug!("cannot watch {}: {e}", dir.display()),
        }
    }
    // What is left of the old set is closed as it drops.
    *watched = next;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::fs::Watcher;
    use crate::ops::fixture::TempTree;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn counting() -> (Notifier, Arc<AtomicUsize>) {
        let bell = Arc::new(AtomicUsize::new(0));
        let rung = Arc::clone(&bell);
        let notifier: Notifier = Arc::new(move || {
            rung.fetch_add(1, Ordering::SeqCst);
        });
        (notifier, bell)
    }

    /// The first event within `within` that `wanted` accepts.
    fn first(watcher: &Watcher, within: Duration, wanted: impl Fn(&WatchEvent) -> bool) -> bool {
        let deadline = Instant::now() + within;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match watcher.events().recv_timeout(left) {
                Ok(event) if wanted(&event) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }

    /// The whole path, through the model's own [`Watcher`]: a watch installed,
    /// a file landing, the bell rung, a debounced burst, the directory taken
    /// away. No skipping: a kqueue has no instance limit to run out of.
    #[test]
    fn a_kqueue_watcher_sees_files_land_and_its_directory_go() {
        let t = TempTree::new("kq-watcher");
        let dir = t.dir("watched");
        let (notifier, bell) = counting();
        let watcher = Watcher::new(notifier).unwrap();
        assert!(watcher.is_active());
        watcher.watch(vec![dir.clone()]);
        // The watch is installed on the watcher's thread; give it the moment
        // it takes before the file it has to see.
        std::thread::sleep(Duration::from_millis(100));

        t.file("watched/landed.txt", b"hi");
        assert!(first(&watcher, Duration::from_secs(5), |e| {
            *e == WatchEvent::Changed(dir.clone())
        }));
        assert!(bell.load(Ordering::SeqCst) > 0, "the notifier was rung");

        // One refresh per DEBOUNCE of the burst, whatever it holds: the runner
        // takes a good part of a second to write 200 files, and that is
        // several windows, each one refresh — never one per file.
        let started = Instant::now();
        for i in 0..200 {
            t.file(format!("watched/burst{i}.txt"), b"x");
        }
        let spread = (started.elapsed().as_millis() / DEBOUNCE.as_millis()) as usize + 2;
        std::thread::sleep(DEBOUNCE * 4);
        let burst = watcher.drain();
        assert!(
            (1..=spread).contains(&burst.len()),
            "200 files over {spread} debounce windows: {burst:?}"
        );
        assert!(burst.iter().all(|e| *e == WatchEvent::Changed(dir.clone())));

        std::fs::remove_dir_all(&dir).unwrap();
        assert!(first(&watcher, Duration::from_secs(5), |e| {
            *e == WatchEvent::Gone(dir.clone())
        }));
    }

    /// A directory taken out of the watch set is closed and says nothing
    /// more; the one that replaced it is heard.
    #[test]
    fn a_directory_no_longer_asked_for_is_not_heard() {
        let t = TempTree::new("kq-rewatch");
        let old = t.dir("old");
        let new = t.dir("new");
        let (notifier, _) = counting();
        let watcher = Watcher::new(notifier).unwrap();
        watcher.watch(vec![old.clone()]);
        std::thread::sleep(Duration::from_millis(100));
        watcher.watch(vec![new.clone()]);
        std::thread::sleep(Duration::from_millis(100));

        t.file("old/ignored.txt", b"x");
        t.file("new/heard.txt", b"x");
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !seen.contains(&WatchEvent::Changed(new.clone())) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            match watcher.events().recv_timeout(left) {
                Ok(event) => seen.push(event),
                Err(_) => break,
            }
        }
        std::thread::sleep(DEBOUNCE * 2);
        seen.extend(watcher.drain());
        assert!(seen.contains(&WatchEvent::Changed(new)), "{seen:?}");
        assert!(
            !seen.contains(&WatchEvent::Changed(old)),
            "the old directory is not watched: {seen:?}"
        );
    }
}
