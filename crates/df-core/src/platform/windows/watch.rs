//! The Windows watcher: `ReadDirectoryChangesW` on the directories on screen,
//! an event to wake the thread, and the same burst debounce as Linux's (see
//! [`crate::fs::Watcher`] for what the model does with the events).
//!
//! Each watched directory is a handle opened for listing with every share
//! mode — so the watch never stops anyone renaming or deleting what is in it
//! — and one overlapped `ReadDirectoryChangesW` pending on it at a time,
//! non-recursive, for names, attributes, sizes, writes and creations. Its
//! completion sets the directory's own event, and the `df-watch` thread sleeps
//! in `WaitForMultipleObjects` over those events and one manual-reset wake
//! event, which [`Backend::wake`] sets when a control message is waiting — the
//! self-pipe's place. Between bursts the thread makes no call at all, so an
//! idle delightfile costs no wake-ups here either.
//!
//! What changed is not read: the model rescans a changed directory whole, so
//! any completed read is `Changed(dir)`, and so is `ERROR_NOTIFY_ENUM_DIR` and
//! a read that overflowed its buffer (both mean "too much to list, look
//! again"). A directory that is deleted fails its pending read (or the next
//! one) with access denied; one that is gone by name at a flush — deleted or
//! renamed away, which a handle does not notice — is found by looking. Either
//! way it is `Gone(dir)`, and its handle closes so the deletion can finish.
//!
//! The rules, as in the other islands of `unsafe`:
//!
//! 1. Every handle is owned by exactly one value that closes it on drop — the
//!    directory's `File`, each event's `OwnedHandle` — so none leaks or is
//!    closed twice.
//! 2. What a pending read writes into — its `OVERLAPPED` and its buffer — is
//!    boxed, so it does not move while the kernel holds its address, and is
//!    freed only after the read has completed or been cancelled and waited
//!    out ([`Watch`]'s `Drop`).
//! 3. Every call's return is checked and turned into
//!    [`std::io::Error::last_os_error`]; nothing is assumed to succeed, and no
//!    `unsafe` escapes the file.

#![allow(unsafe_code)] // ReadDirectoryChangesW, events and waits, on handles owned here

use std::fs::File;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use windows_sys::Win32::Foundation::{HANDLE, WAIT_FAILED, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{
    ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, INFINITE,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::fs::watch::{Control, DEBOUNCE};
use crate::fs::{Notifier, WatchEvent};

/// What a directory is watched for: a name made, removed or renamed (files
/// and folders), attributes, size, last write, creation time.
const FILTER: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
    | FILE_NOTIFY_CHANGE_DIR_NAME
    | FILE_NOTIFY_CHANGE_ATTRIBUTES
    | FILE_NOTIFY_CHANGE_SIZE
    | FILE_NOTIFY_CHANGE_LAST_WRITE
    | FILE_NOTIFY_CHANGE_CREATION;

/// The notification buffer, in 32-bit words: 64 KiB, the most a read on a
/// network share may ask for, and `u32`s because the call wants it
/// `DWORD`-aligned.
const BUFFER_WORDS: usize = 16 * 1024;

/// How many directories can be watched at once: `WaitForMultipleObjects`
/// takes 64 handles, and one is the wake event.
const MAX_WATCHES: usize = 63;

const ERROR_OPERATION_ABORTED: i32 = 995;
const ERROR_IO_INCOMPLETE: i32 = 996;
const ERROR_NOTIFY_ENUM_DIR: i32 = 1022;

/// A manual-reset event, owned.
struct Event(OwnedHandle);

impl Event {
    fn new() -> io::Result<Event> {
        // SAFETY: no security attributes and no name; manual reset, not
        // signalled. The handle, when there is one, is owned by the
        // `OwnedHandle` from here on.
        let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if raw == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a handle this call just created and nothing else
        // holds.
        Ok(Event(unsafe { OwnedHandle::from_raw_handle(raw as _) }))
    }

    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }

    fn set(&self) {
        // SAFETY: an event handle owned by `self`, open for the call.
        unsafe { SetEvent(self.raw()) };
    }

    fn reset(&self) {
        // SAFETY: as `set`.
        unsafe { ResetEvent(self.raw()) };
    }
}

/// What a running [`crate::fs::Watcher`] holds of the backend: the event
/// that wakes the thread when a control message is waiting.
pub(crate) struct Backend {
    wake: Arc<Event>,
}

impl Backend {
    /// Make the wake event and start the `df-watch` thread. The thread reads
    /// `control` and sends on `events`, ringing `notify` after each flushed
    /// burst. Fails only when the event or the thread cannot be had, which
    /// the caller survives.
    pub(crate) fn open(
        control: Receiver<Control>,
        events: Sender<WatchEvent>,
        notify: Notifier,
    ) -> io::Result<(Backend, std::thread::JoinHandle<()>)> {
        let wake = Arc::new(Event::new()?);
        let thread_wake = Arc::clone(&wake);
        let thread = std::thread::Builder::new()
            .name("df-watch".to_string())
            .spawn(move || run(&thread_wake, &control, &events, &notify))?;
        Ok((Backend { wake }, thread))
    }

    /// Interrupt the thread's wait so it reads the control channel.
    pub(crate) fn wake(&self) {
        self.wake.set();
    }
}

/// One watched directory and its pending read.
struct Watch {
    dir: PathBuf,
    handle: File,
    event: Event,
    overlapped: Box<OVERLAPPED>,
    buffer: Box<[u32; BUFFER_WORDS]>,
    /// A read is outstanding: the kernel may still write `overlapped` and
    /// `buffer`.
    pending: bool,
}

/// An `OVERLAPPED` with nothing in it.
fn blank() -> OVERLAPPED {
    // SAFETY: a plain C struct of integers, a union of integers and a
    // pointer, and a handle; all-zero is its documented starting value.
    unsafe { std::mem::zeroed() }
}

impl Watch {
    /// Open `dir` for listing and ask for its first changes.
    fn open(dir: PathBuf) -> io::Result<Watch> {
        let handle = std::fs::OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED)
            .open(&dir)?;
        let mut watch = Watch {
            dir,
            handle,
            event: Event::new()?,
            overlapped: Box::new(blank()),
            buffer: Box::new([0; BUFFER_WORDS]),
            pending: false,
        };
        watch.arm()?;
        Ok(watch)
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    /// Ask for the next batch of changes. Only when no read is pending.
    fn arm(&mut self) -> io::Result<()> {
        debug_assert!(!self.pending);
        self.event.reset();
        *self.overlapped = blank();
        self.overlapped.hEvent = self.event.raw();
        // SAFETY: the directory handle is `self.handle`'s, opened for
        // overlapped listing; the buffer and the `OVERLAPPED` are boxed and
        // owned by `self`, which keeps them until the read is over (rule 2);
        // the byte count is the buffer's own size; with an `OVERLAPPED`, the
        // returned-bytes pointer may be null and no completion routine is
        // given.
        let ok = unsafe {
            ReadDirectoryChangesW(
                self.raw(),
                self.buffer.as_mut_ptr().cast(),
                (BUFFER_WORDS * 4) as u32,
                0,
                FILTER,
                std::ptr::null_mut(),
                &mut *self.overlapped,
                None,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        self.pending = true;
        Ok(())
    }

    /// How the pending read ended, or `None` while it is still pending.
    fn completed(&mut self) -> Option<io::Result<u32>> {
        if !self.pending {
            return None;
        }
        let mut bytes = 0u32;
        // SAFETY: the handle and the `OVERLAPPED` the pending read was issued
        // with, both owned by `self`; `bytes` is a local. Not waiting.
        let ok = unsafe { GetOverlappedResult(self.raw(), &*self.overlapped, &mut bytes, 0) };
        if ok != 0 {
            self.pending = false;
            return Some(Ok(bytes));
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(ERROR_IO_INCOMPLETE) {
            return None;
        }
        self.pending = false;
        Some(Err(e))
    }
}

impl Drop for Watch {
    /// Cancel the pending read and wait for the cancel to land, so that the
    /// buffer and the `OVERLAPPED` are freed only once the kernel is done with
    /// them; the handle and the event close after, as their owners drop.
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        let mut bytes = 0u32;
        // SAFETY: the handle and the `OVERLAPPED` of the read this watch
        // issued. A read that finished meanwhile makes the cancel fail
        // harmlessly; either way the wait returns once it is over, and its
        // event is the one in the `OVERLAPPED`.
        unsafe {
            CancelIoEx(self.raw(), &*self.overlapped);
            GetOverlappedResult(self.raw(), &*self.overlapped, &mut bytes, 1);
        }
        self.pending = false;
    }
}

/// Milliseconds to wait until `deadline`: rounded up, so a remainder under a
/// millisecond does not become a wait of zero and a spin.
fn millis_until(deadline: Instant) -> u32 {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return 0;
    }
    u32::try_from(left.as_millis().max(1)).unwrap_or(INFINITE - 1)
}

/// The watcher thread.
fn run(wake: &Event, control: &Receiver<Control>, events: &Sender<WatchEvent>, notify: &Notifier) {
    let mut watches: Vec<Watch> = Vec::new();
    let mut dirty: Vec<PathBuf> = Vec::new();
    let mut gone: Vec<PathBuf> = Vec::new();
    let mut deadline: Option<Instant> = None;

    loop {
        let mut handles = vec![wake.raw()];
        handles.extend(watches.iter().filter(|w| w.pending).map(|w| w.event.raw()));
        let timeout = deadline.map_or(INFINITE, millis_until);
        // SAFETY: `handles` holds at most 64 event handles (the wake event
        // and at most `MAX_WATCHES`), each owned by a value alive across the
        // call; the count passed is its length.
        let woke =
            unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, timeout) };
        if woke == WAIT_FAILED {
            log::warn!(
                "the directory watch failed ({}); giving up on auto-refresh",
                io::Error::last_os_error()
            );
            return;
        }

        if woke == WAIT_OBJECT_0 {
            // Reset before draining: a message sent after the drain sets the
            // event again, and the next wait returns at once for it.
            wake.reset();
            for message in control.try_iter() {
                match message {
                    Control::Watch(dirs) => set_watches(&mut watches, dirs),
                    Control::Stop => return,
                }
            }
        }

        // Whatever woke the thread, every watch is looked at: the wait reports
        // only the lowest signalled handle, and a busy directory must not
        // starve the others.
        watches.retain_mut(|watch| {
            let outcome = match watch.completed() {
                None => return true,
                Some(outcome) => outcome,
            };
            match outcome {
                Ok(_) => {}
                Err(e) if e.raw_os_error() == Some(ERROR_NOTIFY_ENUM_DIR) => {}
                // Only a watch's own drop cancels, and it is not here then;
                // arm it again.
                Err(e) if e.raw_os_error() == Some(ERROR_OPERATION_ABORTED) => {}
                Err(e) => {
                    log::debug!("{}: the watch ended: {e}", watch.dir.display());
                    push_once(&mut gone, &watch.dir);
                    return false;
                }
            }
            push_once(&mut dirty, &watch.dir);
            match watch.arm() {
                Ok(()) => true,
                Err(e) => {
                    log::debug!("{}: cannot watch again: {e}", watch.dir.display());
                    push_once(&mut gone, &watch.dir);
                    false
                }
            }
        });

        if (!dirty.is_empty() || !gone.is_empty()) && deadline.is_none() {
            deadline = Some(Instant::now() + DEBOUNCE);
        }

        // The deadline, not a sliding window, as on Linux: a directory under
        // continuous write still refreshes every DEBOUNCE.
        if deadline.is_some_and(|d| Instant::now() >= d) {
            deadline = None;
            // A handle does not notice its directory deleted by name at once,
            // or renamed away; the name does.
            watches.retain(|watch| {
                let there = std::fs::symlink_metadata(&watch.dir).is_ok_and(|m| m.is_dir());
                if !there {
                    push_once(&mut gone, &watch.dir);
                }
                there
            });
            dirty.retain(|dir| !gone.contains(dir));
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

fn push_once(list: &mut Vec<PathBuf>, dir: &PathBuf) {
    if !list.contains(dir) {
        list.push(dir.clone());
    }
}

/// Make the watched set exactly `dirs`: a directory already watched keeps
/// its watch, a new one is opened, and one no longer asked for is dropped,
/// which cancels its read and closes its handle.
fn set_watches(watches: &mut Vec<Watch>, dirs: Vec<PathBuf>) {
    let mut next: Vec<Watch> = Vec::new();
    for dir in dirs {
        if next.iter().any(|watch| watch.dir == dir) {
            continue;
        }
        if next.len() == MAX_WATCHES {
            log::debug!("not watching {}: {MAX_WATCHES} already", dir.display());
            continue;
        }
        if let Some(at) = watches.iter().position(|watch| watch.dir == dir) {
            next.push(watches.swap_remove(at));
            continue;
        }
        match Watch::open(dir.clone()) {
            Ok(watch) => next.push(watch),
            // Gone between the navigation and this call, or not a directory:
            // no auto-refresh for that pane, and nothing else changes.
            Err(e) => log::debug!("cannot watch {}: {e}", dir.display()),
        }
    }
    *watches = next;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use crate::fs::{no_notifier, WatchEvent, Watcher};
    use crate::test_support::TempTree;
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// Keep making files in `dir` until the watcher reports it changed, which
    /// is how a test knows the watch is armed on the watcher's thread; `None`
    /// if nothing arrives by the deadline.
    fn armed(watcher: &Watcher, dir: &Path, within: Duration) -> Option<Duration> {
        let start = Instant::now();
        let mut round = 0;
        while start.elapsed() < within {
            std::fs::write(dir.join(format!("arming-{round}.txt")), b"x").unwrap();
            round += 1;
            if let Ok(event) = watcher.events().recv_timeout(Duration::from_millis(250)) {
                if event == WatchEvent::Changed(dir.to_path_buf()) {
                    return Some(start.elapsed());
                }
            }
        }
        None
    }

    /// A file landing in a watched folder is a change within a second, once
    /// the watch is armed.
    #[test]
    fn a_file_landing_is_a_change_within_a_second() {
        let t = TempTree::new("win-watch");
        let watcher = Watcher::new(no_notifier()).unwrap();
        assert!(watcher.is_active());
        watcher.watch(vec![t.path().to_path_buf()]);
        armed(&watcher, t.path(), Duration::from_secs(10)).expect("the watch never armed");
        while watcher.events().try_recv().is_ok() {}

        let landed = Instant::now();
        std::fs::write(t.join("landed.txt"), b"hi").unwrap();
        let event = watcher
            .events()
            .recv_timeout(Duration::from_secs(1))
            .expect("no change within a second");
        assert_eq!(event, WatchEvent::Changed(t.path().to_path_buf()));
        eprintln!("changed after {:?}", landed.elapsed());
    }

    /// A watched folder deleted from under the watch is gone, and the watch
    /// lets it go: the folder does not linger.
    #[test]
    fn a_watched_folder_deleted_is_gone() {
        let t = TempTree::new("win-watch-gone");
        let doomed = t.dir("doomed");
        let watcher = Watcher::new(no_notifier()).unwrap();
        watcher.watch(vec![doomed.clone()]);
        armed(&watcher, &doomed, Duration::from_secs(10)).expect("the watch never armed");

        std::fs::remove_dir_all(&doomed).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut gone = false;
        while !gone && Instant::now() < deadline {
            if let Ok(WatchEvent::Gone(dir)) =
                watcher.events().recv_timeout(Duration::from_millis(250))
            {
                gone = dir == doomed;
            }
        }
        assert!(gone, "no Gone within five seconds");
        let until = Instant::now() + Duration::from_secs(5);
        while doomed.exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!doomed.exists(), "the watch let the folder go");
    }

    /// Watching a new set drops the old watches, and a folder that is not
    /// there is skipped without stopping the rest.
    #[test]
    fn the_watched_set_is_replaced_and_a_missing_folder_skipped() {
        let t = TempTree::new("win-watch-set");
        let a = t.dir("a");
        let b = t.dir("b");
        let watcher = Watcher::new(no_notifier()).unwrap();
        watcher.watch(vec![a.clone()]);
        armed(&watcher, &a, Duration::from_secs(10)).expect("a never armed");
        watcher.watch(vec![t.join("missing"), b.clone()]);
        armed(&watcher, &b, Duration::from_secs(10)).expect("b never armed");
        while watcher.events().try_recv().is_ok() {}
        std::fs::write(a.join("unwatched.txt"), b"x").unwrap();
        std::fs::write(b.join("watched.txt"), b"x").unwrap();
        let mut seen = Vec::new();
        while let Ok(event) = watcher.events().recv_timeout(Duration::from_millis(500)) {
            seen.push(event);
        }
        assert!(seen.contains(&WatchEvent::Changed(b.clone())), "{seen:?}");
        assert!(!seen.contains(&WatchEvent::Changed(a.clone())), "{seen:?}");
    }
}
