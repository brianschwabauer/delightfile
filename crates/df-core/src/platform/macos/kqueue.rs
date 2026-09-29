//! The kqueue syscalls behind the macOS watcher ([`super::watch`]): a kqueue,
//! a directory opened only to be watched, a self-pipe to wake the thread, and
//! a wait that hands back owned events.
//!
//! kqueue rather than FSEvents (`plans/other-platforms/02-macos.md`): the
//! watcher's events are already coarse — "this directory changed", "this
//! directory is gone" — and every symbol kqueue needs is in `libc`, where
//! FSEvents would need CoreServices bindings and a run loop for a precision
//! the model does not use. One descriptor per watched directory is nothing at
//! the handful of directories on screen.
//!
//! The same three rules as the Linux island (`platform/linux/inotify.rs`):
//!
//! 1. Every raw fd is owned by exactly one struct with a `Drop` that closes it,
//!    so there is no path where a descriptor leaks or gets double-closed. A
//!    watch needs no removing: closing its directory's descriptor takes its
//!    registration out of every kqueue.
//! 2. Events come back as `libc::kevent` structs the kernel wrote into a
//!    buffer of exactly that type, and leave this file as owned copies of the
//!    three fields anything reads — `repr(packed)` fields are copied out,
//!    never borrowed.
//! 3. Every syscall's return value is checked and turned into
//!    [`std::io::Error::last_os_error`]; nothing is assumed to succeed.

// The crate-wide lint (workspace `unsafe_code = "warn"`) is on so that each
// file like this one is a deliberate, single-file exception.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

/// What a watched directory reports: an entry made, removed or renamed in it
/// (`NOTE_WRITE`, and `NOTE_EXTEND` which some filesystems send beside it),
/// its own mode or owner changed (`NOTE_ATTRIB`), and the three ways it can
/// stop being there — deleted, renamed away, or its volume unmounted
/// (`NOTE_DELETE`, `NOTE_RENAME`, `NOTE_REVOKE`).
///
/// A directory hears nothing of a file inside it being rewritten in place or
/// having its own mode changed; only the directory's list of names is watched.
pub const WATCH_FLAGS: u32 = libc::NOTE_WRITE
    | libc::NOTE_EXTEND
    | libc::NOTE_ATTRIB
    | libc::NOTE_DELETE
    | libc::NOTE_RENAME
    | libc::NOTE_REVOKE;

/// The flags among [`WATCH_FLAGS`] that mean the directory itself is gone.
pub const GONE_FLAGS: u32 = libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE;

/// How many events one wait can hand back. A burst larger than this is read
/// in several waits, each immediate.
const EVENTS_PER_WAIT: usize = 64;

/// One event, copied out of the kernel's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// The descriptor it is about: a watched directory's, or the pipe's.
    pub ident: usize,
    /// `EVFILT_VNODE` for a directory, `EVFILT_READ` for the pipe.
    pub filter: i16,
    /// For a directory, which of [`WATCH_FLAGS`] happened.
    pub fflags: u32,
}

impl Event {
    /// The watched directory itself went away.
    pub fn is_gone(&self) -> bool {
        self.fflags & GONE_FLAGS != 0
    }
}

/// Set close-on-exec on `fd`, so a shelled-out `fd`, `rg` or opener does not
/// inherit it.
fn cloexec(fd: i32) -> io::Result<()> {
    // SAFETY: two integers and a flag word; no memory is passed.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A kqueue. Closes its descriptor on drop, which ends every registration on
/// it.
#[derive(Debug)]
pub struct Kqueue {
    fd: i32,
}

impl Kqueue {
    pub fn new() -> io::Result<Kqueue> {
        // SAFETY: no arguments; returns a descriptor or -1.
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let kqueue = Kqueue { fd };
        cloexec(fd)?;
        Ok(kqueue)
    }

    /// Register one change and nothing else.
    fn change(&self, change: libc::kevent) -> io::Result<()> {
        // SAFETY: one live `kevent` in, no event list out, no timeout; the
        // kernel reads exactly `nchanges` structs from the pointer.
        let rc = unsafe {
            libc::kevent(
                self.fd,
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Watch `dir` for [`WATCH_FLAGS`]. `EV_CLEAR`, so a directory written to
    /// a thousand times between two waits is one event, not a thousand.
    pub fn watch_dir(&self, dir: &WatchedDir) -> io::Result<()> {
        self.change(libc::kevent {
            ident: dir.fd as libc::uintptr_t,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: WATCH_FLAGS,
            data: 0,
            udata: std::ptr::null_mut(),
        })
    }

    /// Wake on `fd` becoming readable: the self-pipe.
    pub fn watch_read(&self, fd: i32) -> io::Result<()> {
        self.change(libc::kevent {
            ident: fd as libc::uintptr_t,
            filter: libc::EVFILT_READ,
            flags: libc::EV_ADD,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        })
    }

    /// Wait for events, or for `timeout` to pass. `None` waits forever, which
    /// is what the watcher does with nothing pending — a thread asleep in
    /// `kevent` costs nothing. A signal is not a failure: the wait starts again
    /// with what is left of the time.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<Vec<Event>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            let spec = left.map(|d| libc::timespec {
                tv_sec: d.as_secs() as libc::time_t,
                tv_nsec: d.subsec_nanos() as libc::c_long,
            });
            let spec_ptr = spec
                .as_ref()
                .map_or(std::ptr::null(), |s| s as *const libc::timespec);
            let empty = libc::kevent {
                ident: 0,
                filter: 0,
                flags: 0,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            let mut buffer = [empty; EVENTS_PER_WAIT];
            // SAFETY: no change list; `buffer` is a live array of exactly
            // `EVENTS_PER_WAIT` structs, and `spec_ptr` is null or points at
            // a `timespec` that outlives the call.
            let n = unsafe {
                libc::kevent(
                    self.fd,
                    std::ptr::null(),
                    0,
                    buffer.as_mut_ptr(),
                    EVENTS_PER_WAIT as libc::c_int,
                    spec_ptr,
                )
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            return Ok(buffer[..n as usize]
                .iter()
                .map(|raw| Event {
                    ident: raw.ident,
                    filter: raw.filter,
                    fflags: raw.fflags,
                })
                .collect());
        }
    }
}

impl Drop for Kqueue {
    fn drop(&mut self) {
        // SAFETY: `self.fd` came from `kqueue`, is owned solely by this
        // struct, and drop runs once.
        unsafe { libc::close(self.fd) };
    }
}

/// A directory opened only to be watched: `O_EVTONLY`, which reads nothing
/// and, unlike an ordinary descriptor, does not keep its volume from being
/// unmounted — the unmount arrives as `NOTE_REVOKE` instead.
#[derive(Debug)]
pub struct WatchedDir {
    fd: i32,
}

impl WatchedDir {
    /// Open `dir`. `O_DIRECTORY` refuses a path that is not one, which turns
    /// a race (the directory replaced by a file between the navigation and
    /// this call) into an error rather than a watch on the wrong thing.
    pub fn open(dir: &Path) -> io::Result<WatchedDir> {
        let c_path = CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        // SAFETY: `c_path` is a NUL-terminated string that outlives the call.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_EVTONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(WatchedDir { fd })
    }

    /// The descriptor, which is also the ident its events carry.
    pub fn fd(&self) -> i32 {
        self.fd
    }
}

impl Drop for WatchedDir {
    fn drop(&mut self) {
        // SAFETY: `self.fd` came from `open`, is owned solely by this struct,
        // and drop runs once. Closing it removes its kqueue registrations.
        unsafe { libc::close(self.fd) };
    }
}

/// A self-pipe, the way to wake a thread out of `kevent` from another thread.
/// Made with `pipe` and two `fcntl`s each, since Apple's libc has no `pipe2`:
/// close-on-exec, and non-blocking so a wake on a full pipe and a drain of an
/// empty one both return at once.
#[derive(Debug)]
pub struct Pipe {
    read_fd: i32,
    write_fd: i32,
}

impl Pipe {
    pub fn new() -> io::Result<Pipe> {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a two-element array, which is what `pipe` writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // Owned from here, so an `fcntl` that fails below closes both.
        let pipe = Pipe {
            read_fd: fds[0],
            write_fd: fds[1],
        };
        for fd in fds {
            cloexec(fd)?;
            // SAFETY: integers and flag words only.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: as above.
            if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(pipe)
    }

    pub fn read_fd(&self) -> i32 {
        self.read_fd
    }

    /// Wake whoever is waiting on the read end. Safe from any thread and any
    /// number of times; a full pipe means a wake-up is already pending.
    pub fn wake(&self) {
        let byte = 1u8;
        // SAFETY: writing one byte from a live local into an owned descriptor.
        unsafe {
            libc::write(
                self.write_fd,
                std::ptr::addr_of!(byte) as *const libc::c_void,
                1,
            )
        };
    }

    /// Throw away pending wake-up bytes so the next wait blocks again.
    pub fn drain(&self) {
        let mut buffer = [0u8; 64];
        loop {
            // SAFETY: reading into a live local buffer of exactly this size.
            let n = unsafe {
                libc::read(
                    self.read_fd,
                    buffer.as_mut_ptr() as *mut libc::c_void,
                    buffer.len(),
                )
            };
            if n < buffer.len() as isize {
                break;
            }
        }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: both descriptors came from `pipe`, are owned solely here,
        // and drop runs once.
        unsafe {
            libc::close(self.read_fd);
            libc::close(self.write_fd);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::TempTree;

    /// Nothing pending is an empty wait, not a hang: the timeout is honoured.
    #[test]
    fn a_wait_with_nothing_pending_times_out_empty() {
        let kq = Kqueue::new().unwrap();
        let started = Instant::now();
        assert!(kq.wait(Some(Duration::from_millis(30))).unwrap().is_empty());
        assert!(started.elapsed() >= Duration::from_millis(25));
    }

    /// The pipe wakes the wait, and a drained pipe does not wake it again.
    #[test]
    fn the_pipe_wakes_a_wait_once() {
        let kq = Kqueue::new().unwrap();
        let pipe = Pipe::new().unwrap();
        kq.watch_read(pipe.read_fd()).unwrap();
        pipe.wake();
        pipe.wake();
        let events = kq.wait(Some(Duration::from_secs(5))).unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].filter, libc::EVFILT_READ);
        assert_eq!(events[0].ident, pipe.read_fd() as usize);
        pipe.drain();
        assert!(kq.wait(Some(Duration::from_millis(20))).unwrap().is_empty());
    }

    /// A file made in a watched directory is a write to it; the directory
    /// removed is a delete of it; and a path that is not a directory is not
    /// watched at all.
    #[test]
    fn a_watched_directory_reports_a_new_name_and_its_own_removal() {
        let t = TempTree::new("kqueue");
        let dir = t.dir("watched");
        let kq = Kqueue::new().unwrap();
        let watched = WatchedDir::open(&dir).unwrap();
        kq.watch_dir(&watched).unwrap();

        t.file("watched/new.txt", b"x");
        let events = kq.wait(Some(Duration::from_secs(5))).unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].ident, watched.fd() as usize);
        assert_eq!(events[0].filter, libc::EVFILT_VNODE);
        assert!(events[0].fflags & libc::NOTE_WRITE != 0, "{events:?}");
        assert!(!events[0].is_gone());

        std::fs::remove_dir_all(&dir).unwrap();
        let events = kq.wait(Some(Duration::from_secs(5))).unwrap();
        assert!(events.iter().any(Event::is_gone), "{events:?}");

        let file = t.file("plain.txt", b"x");
        assert!(WatchedDir::open(&file).is_err());
    }

    /// A directory renamed away is gone from where it was watched.
    #[test]
    fn a_watched_directory_renamed_away_is_gone() {
        let t = TempTree::new("kqueue-rename");
        let dir = t.dir("before");
        let kq = Kqueue::new().unwrap();
        let watched = WatchedDir::open(&dir).unwrap();
        kq.watch_dir(&watched).unwrap();
        std::fs::rename(&dir, t.join("after")).unwrap();
        let events = kq.wait(Some(Duration::from_secs(5))).unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.fflags & libc::NOTE_RENAME != 0 && e.is_gone()),
            "{events:?}"
        );
    }
}
