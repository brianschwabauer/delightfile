//! Waiting on the child's pipes with a deadline. **The crate's second island of
//! `unsafe`**, after `fs::inotify`, and written to the same three rules.
//!
//! A file manager that hangs is worse than one that fails. An `ssh` subprocess
//! can stop answering for reasons none of which produce an error on the pipe: a
//! laptop that closed its lid mid-transfer, a server that went away without a
//! FIN, a network that is dropping packets into a hole. A plain `read` on the
//! child's stdout waits for that forever, and the connection thread with it, and
//! then every listing queued behind it. So every read and every write in
//! `vfs::conn` happens after a `poll` with a timeout, and a timeout is an
//! error the user gets told about (PLAN §7.6's remote panes must fail visibly,
//! not silently stop being a file manager).
//!
//! `std` has no timed read on a pipe. `Read::read` cannot take a deadline,
//! `set_read_timeout` is a socket method, and the only portable-in-practice
//! answer on Linux is `poll(2)` — which is one syscall and a `pollfd` struct, so
//! the "rewriting is impractical" bar for a new dependency (PLAN §1) is nowhere
//! near met.
//!
//! The rules, unchanged from `fs::inotify`:
//!
//! 1. Nothing here **owns** a descriptor. Every function borrows a raw fd whose
//!    lifetime belongs to a `std::process::ChildStdin`/`ChildStdout`/
//!    `ChildStderr`, so there is no path where this file closes something twice
//!    or leaks something once. Callers pass a fd they are holding a live handle
//!    to for the duration of the call.
//! 2. Every syscall's return is checked and turned into an
//!    [`std::io::Error::last_os_error`]. `EINTR` is not a failure — a profiler
//!    or a `SIGWINCH` is allowed to interrupt a wait — so it reports "nothing
//!    ready" and the caller loops against its own deadline.
//! 3. No `unsafe` escapes the file. Everything above it sees `io::Result<bool>`.

// Deliberate, contained, and the second and last of these in df-core.
#![allow(unsafe_code)]

use std::io;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

/// Whether the child's pipes can be waited on with a deadline here: they can,
/// with `poll`.
pub const AVAILABLE: bool = true;

/// The raw descriptor of one of a child's pipes, for the calls below. The
/// caller keeps the pipe alive for as long as it uses the number.
pub fn fd(pipe: &impl AsRawFd) -> i32 {
    pipe.as_raw_fd()
}

/// How many milliseconds to hand `poll`, from a `Duration`.
///
/// Rounded **up** to at least one, because a zero timeout turns `poll` into a
/// busy loop: the caller's deadline arithmetic can legitimately produce a
/// sub-millisecond remainder, and spinning on it is how a "timeout" becomes
/// 100% of a core.
fn millis(timeout: Duration) -> i32 {
    let ms = timeout.as_millis();
    if ms == 0 {
        1
    } else {
        i32::try_from(ms).unwrap_or(i32::MAX)
    }
}

/// Time left until `deadline`, or `None` if it has passed.
pub fn remaining(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

/// Wait until either descriptor has something to read, or the timeout expires.
///
/// Two, because the connection thread watches the child's stdout (the protocol)
/// and its stderr (ssh's complaints) at once. Watching only stdout is the bug
/// where an authentication failure looks like a hang: `ssh` writes "Permission
/// denied" to stderr, exits, and the only signal on stdout is an EOF whose cause
/// is sitting unread in the other pipe.
///
/// Returns `(stdout_ready, stderr_ready)`. `(false, false)` means the timeout
/// elapsed — or a signal interrupted the wait, which the caller cannot tell
/// apart and does not need to: it re-checks its own deadline either way.
pub fn poll_read2(stdout: i32, stderr: i32, timeout: Duration) -> io::Result<(bool, bool)> {
    let mut fds = [
        libc::pollfd {
            fd: stdout,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: stderr,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: `fds` is a live array of exactly the two entries claimed, and the
    // descriptors are borrowed from handles the caller is holding.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, millis(timeout)) };
    if rc < 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::Interrupted {
            return Ok((false, false));
        }
        return Err(e);
    }
    // POLLHUP and POLLERR both mean "read it and find out" — a closed pipe is
    // reported by a `read` of 0, which is where EOF belongs. So any revent
    // counts as ready.
    Ok((fds[0].revents != 0, fds[1].revents != 0))
}

/// Wait until a descriptor will accept a write, or the timeout expires.
///
/// The mirror of the read case, and needed for the same reason: an upload
/// writing 32 KiB chunks into a 64 KiB pipe blocks the moment the server stops
/// draining it, and a blocked `write_all` is just as unkillable as a blocked
/// `read`.
pub fn poll_write(fd: i32, timeout: Duration) -> io::Result<bool> {
    let mut fds = [libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    }];
    // SAFETY: a live one-element array, and a descriptor borrowed from a handle
    // the caller holds.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, millis(timeout)) };
    if rc < 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(e);
    }
    Ok(fds[0].revents != 0)
}

/// Put a descriptor in non-blocking mode.
///
/// Only the child's **stdin** gets this. Its stdout stays blocking, because a
/// `read` that follows a `poll` saying "readable" returns immediately anyway,
/// and leaving it blocking keeps the read path one syscall simpler. Stdin is
/// different: `poll` says "writable" when *some* room exists, and a 32 KiB write
/// into 8 KiB of room would block for the rest — so the write path needs partial
/// writes, and partial writes need `O_NONBLOCK`.
pub fn set_nonblocking(fd: i32) -> io::Result<()> {
    // SAFETY: `F_GETFL` takes no pointer and returns the flag word or -1.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `F_SETFL` with an int argument; the descriptor is borrowed from a
    // live handle.
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
