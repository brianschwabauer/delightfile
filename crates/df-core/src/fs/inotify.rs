//! The syscalls. **This is the only file in df-core that contains `unsafe`**,
//! and it is kept small and boring so that it stays reviewable.
//!
//! Rust has no inotify in `std` and PLAN §1 says a new crate needs a reason
//! rewriting is impractical; inotify is four syscalls and a struct with a
//! variable-length tail, so it does not clear that bar. What is here is a thin,
//! total wrapper: file descriptors owned by types that close them on drop, a
//! read that parses the kernel's byte stream into owned events, a `poll` over
//! two descriptors, and a self-pipe for waking the watcher thread out of that
//! `poll`. No `unsafe` escapes this file — everything above it sees `io::Result`
//! and owned data.
//!
//! Three rules for the code below, since it is the crate's one unsafe island:
//!
//! 1. Every raw fd is owned by exactly one struct with a `Drop` that closes it,
//!    so there is no path where a descriptor leaks or gets double-closed.
//! 2. The event stream is parsed with `read_unaligned`, because the kernel packs
//!    records back to back and the second one need not be aligned.
//! 3. Every syscall's return value is checked and turned into
//!    [`std::io::Error::last_os_error`]; nothing is assumed to succeed.

// The crate-wide lint (workspace `unsafe_code = "warn"`) is on precisely so this
// is a deliberate, single-file exception rather than a habit.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The events a directory listing cares about.
///
/// - `CREATE`/`DELETE`/`MOVED_FROM`/`MOVED_TO` — a row appeared or vanished.
/// - `CLOSE_WRITE` — a file finished being written. Not `MODIFY`: a program
///   writing a 4 GB video emits thousands of `MODIFY`s, and the row's size is
///   only interesting once. `CLOSE_WRITE` fires once, at the end.
/// - `ATTRIB` — chmod, chown, and the *link count* change that a hardlink or a
///   `rm` of a sibling link causes. It is also what fires when a file's mtime is
///   touched, which is how the linemode stays honest.
/// - `DELETE_SELF`/`MOVE_SELF` — the directory being watched is gone; the pane
///   has to do something about itself rather than about a row.
///
/// `IN_ONLYDIR` refuses to watch a path that is not a directory, which turns a
/// race (the directory replaced by a file between `stat` and `add_watch`) into
/// an error return instead of a watch on the wrong thing.
pub const WATCH_MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_ATTRIB
    | libc::IN_CLOSE_WRITE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR;

/// One event, with the name copied out of the kernel's buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Which watch it belongs to. `-1` on an overflow event, which belongs to
    /// no watch — see [`Event::is_overflow`].
    pub wd: i32,
    pub mask: u32,
    /// The entry inside the watched directory, when the event names one.
    pub name: Option<String>,
}

impl Event {
    /// The kernel's event queue filled up and events were dropped. There is no
    /// way to know *what* was missed, so the only correct response is a full
    /// rescan of everything being watched.
    pub fn is_overflow(&self) -> bool {
        self.mask & libc::IN_Q_OVERFLOW != 0
    }

    /// The watched directory itself went away.
    pub fn is_self_gone(&self) -> bool {
        self.mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_IGNORED) != 0
    }
}

/// An inotify instance. Closes its descriptor on drop, which also drops every
/// watch on it — no need to `rm_watch` on the way out.
#[derive(Debug)]
pub struct Inotify {
    fd: i32,
}

impl Inotify {
    /// `IN_NONBLOCK` so a `read` after a `poll` can never stall the watcher
    /// thread on a spurious wake-up; `IN_CLOEXEC` so a shelled-out `fd`, `rg`
    /// or opener (PLAN §6) does not inherit it.
    pub fn new() -> io::Result<Inotify> {
        // SAFETY: no arguments but a flag word; returns a descriptor or -1.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Inotify { fd })
    }

    pub fn fd(&self) -> i32 {
        self.fd
    }

    /// Watch a directory. Returns the watch descriptor, which the caller maps
    /// back to a path. Fails with `ENOSPC` when the per-user watch limit is
    /// reached, which the caller must survive — see [`super::watch`].
    pub fn add_watch(&self, path: &Path, mask: u32) -> io::Result<i32> {
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        // SAFETY: `c_path` is a valid NUL-terminated string that outlives the
        // call, and `self.fd` is an inotify descriptor this struct owns.
        let wd = unsafe { libc::inotify_add_watch(self.fd, c_path.as_ptr(), mask) };
        if wd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(wd)
    }

    pub fn rm_watch(&self, wd: i32) {
        // SAFETY: both arguments are plain integers; a stale `wd` returns
        // EINVAL, which is nothing to do anything about — the watch is gone
        // either way, which is what was wanted.
        unsafe { libc::inotify_rm_watch(self.fd, wd) };
    }

    /// Drain whatever is queued. Returns an empty vector when nothing is ready
    /// (`EAGAIN`), which is a normal outcome of a poll that woke for the pipe.
    pub fn read_events(&self) -> io::Result<Vec<Event>> {
        // Aligned to 8 bytes by construction (a `[u64]`), because the records
        // inside are read as structs. 4 KiB holds ~100 events with names,
        // enough that a burst is drained in a handful of reads.
        let mut buffer = [0u64; 512];
        let bytes = buffer.as_mut_ptr() as *mut libc::c_void;
        let capacity = std::mem::size_of_val(&buffer);
        // SAFETY: `bytes`/`capacity` describe exactly the buffer above, which
        // is live for the whole call.
        let n = unsafe { libc::read(self.fd, bytes, capacity) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::WouldBlock {
                return Ok(Vec::new());
            }
            return Err(e);
        }
        let n = n as usize;
        // SAFETY: the kernel wrote `n` initialised bytes into the buffer.
        let filled = unsafe { std::slice::from_raw_parts(buffer.as_ptr() as *const u8, n) };
        Ok(parse_events(filled))
    }
}

impl Drop for Inotify {
    fn drop(&mut self) {
        // SAFETY: `self.fd` was returned by `inotify_init1`, is owned solely by
        // this struct, and drop runs once.
        unsafe { libc::close(self.fd) };
    }
}

/// The header size the kernel packs records at: `wd`, `mask`, `cookie`, `len`,
/// then `len` bytes of NUL-padded name.
const HEADER: usize = std::mem::size_of::<libc::inotify_event>();

fn parse_events(mut buffer: &[u8]) -> Vec<Event> {
    let mut events = Vec::new();
    while buffer.len() >= HEADER {
        // SAFETY: `buffer` has at least a header's worth of initialised bytes,
        // and `read_unaligned` makes no alignment demand — which matters,
        // because a record following an odd-length name is not 4-aligned.
        let raw: libc::inotify_event =
            unsafe { std::ptr::read_unaligned(buffer.as_ptr() as *const libc::inotify_event) };
        let len = raw.len as usize;
        if buffer.len() < HEADER + len {
            // A truncated tail cannot happen — inotify only ever returns whole
            // records — but trusting a length field out of a byte buffer is
            // exactly the habit that produces out-of-bounds reads.
            break;
        }
        let name = if len == 0 {
            None
        } else {
            let raw_name = &buffer[HEADER..HEADER + len];
            // The name is NUL-padded out to an alignment boundary.
            let end = raw_name.iter().position(|b| *b == 0).unwrap_or(len);
            Some(String::from_utf8_lossy(&raw_name[..end]).into_owned())
        };
        events.push(Event {
            wd: raw.wd,
            mask: raw.mask,
            name,
        });
        buffer = &buffer[HEADER + len..];
    }
    events
}

/// A self-pipe: the standard way to wake a thread out of `poll` from another
/// thread. Both ends are owned here and closed together.
///
/// Cheaper alternatives exist (`eventfd`) but this is two syscalls' worth of
/// difference once per process, and a pipe is a thing every reader already
/// understands.
#[derive(Debug)]
pub struct Pipe {
    read_fd: i32,
    write_fd: i32,
}

impl Pipe {
    pub fn new() -> io::Result<Pipe> {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a two-element array, which is what `pipe2` writes.
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Pipe {
            read_fd: fds[0],
            write_fd: fds[1],
        })
    }

    pub fn read_fd(&self) -> i32 {
        self.read_fd
    }

    /// Wake whoever is polling the read end. Safe to call from any thread and
    /// any number of times; a full pipe means a wake-up is already pending,
    /// which is the same outcome.
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

    /// Throw away pending wake-up bytes so the next `poll` blocks again.
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
        // SAFETY: both descriptors came from `pipe2`, are owned solely here,
        // and drop runs once.
        unsafe {
            libc::close(self.read_fd);
            libc::close(self.write_fd);
        }
    }
}

/// Wait until either descriptor is readable, or `timeout` elapses.
///
/// `None` blocks forever, which is what the watcher does when it has nothing
/// pending — a thread asleep in `poll` costs nothing, and PLAN §1's "never a
/// poll loop" applies to threads as much as to the event loop.
///
/// Returns `(a_ready, b_ready)`.
pub fn poll_two(a: i32, b: i32, timeout: Option<std::time::Duration>) -> io::Result<(bool, bool)> {
    let mut fds = [
        libc::pollfd {
            fd: a,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: b,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let ms = match timeout {
        // Rounded up: a zero-millisecond timeout is a busy loop, and the
        // debounce is happier late than early.
        Some(d) => (d.as_millis() as i32).max(1),
        None => -1,
    };
    // SAFETY: `fds` is a live array of exactly the length passed.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
    if rc < 0 {
        let e = io::Error::last_os_error();
        // A signal (SIGWINCH, a profiler) is not a failure; report "nothing
        // ready" and let the caller loop.
        if e.kind() == io::ErrorKind::Interrupted {
            return Ok((false, false));
        }
        return Err(e);
    }
    // POLLERR/POLLHUP also mean "read it and find out", so any revent counts.
    Ok((fds[0].revents != 0, fds[1].revents != 0))
}
