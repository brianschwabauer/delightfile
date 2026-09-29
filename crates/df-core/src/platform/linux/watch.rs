//! The Linux watcher: inotify on the directories on screen, a self-pipe to wake
//! the thread, and the burst debounce (see [`crate::fs::Watcher`] for what the
//! model does with the events).
//!
//! The watcher thread sleeps in `poll` between bursts (see
//! [`super::inotify`]), so an idle delightfile costs zero wake-ups — PLAN §1's
//! idle-cost constraint applies to worker threads too.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

use super::inotify::{self, Inotify, Pipe, WATCH_MASK};
use crate::fs::watch::{Control, DEBOUNCE};
use crate::fs::{Notifier, WatchEvent};

/// What a running [`crate::fs::Watcher`] holds of the backend: the pipe that
/// interrupts the thread's `poll` when a control message is waiting.
pub(crate) struct Backend {
    pipe: Arc<Pipe>,
}

impl Backend {
    /// Open inotify and the wake-up pipe and start the `df-watch` thread. The
    /// thread reads `control` and sends on `events`, ringing `notify` after
    /// each flushed burst. Fails when inotify cannot be had — the instance
    /// limit, a kernel without it — which the caller survives.
    pub(crate) fn open(
        control: Receiver<Control>,
        events: Sender<WatchEvent>,
        notify: Notifier,
    ) -> io::Result<(Backend, std::thread::JoinHandle<()>)> {
        let inotify = Inotify::new()?;
        let pipe = Arc::new(Pipe::new()?);

        let thread_pipe = Arc::clone(&pipe);
        let thread = std::thread::Builder::new()
            .name("df-watch".to_string())
            .spawn(move || run(inotify, thread_pipe, control, events, notify))?;

        Ok((Backend { pipe }, thread))
    }

    /// Interrupt the thread's `poll` so it reads the control channel.
    pub(crate) fn wake(&self) {
        self.pipe.wake();
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
