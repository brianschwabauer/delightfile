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
//! [`Pipes`] holds the child's three pipes and is what the transport calls:
//! one wait-and-read, one wait-and-write, and a last look at stderr. Windows
//! has the same type with reader threads behind it, since its pipes cannot be
//! polled; here it is `poll` and the reads and writes that follow it.
//!
//! The rules, unchanged from `fs::inotify`:
//!
//! 1. Nothing here **owns** a raw descriptor. The pipes are owned by [`Pipes`]
//!    as the `std` handles they arrived as (`ChildStdin`, `ChildStdout`,
//!    `ChildStderr`), which close them on drop, and every call below borrows a
//!    descriptor from one of them for its duration, so there is no path where
//!    this file closes something twice or leaks something once.
//! 2. Every syscall's return is checked and turned into an
//!    [`std::io::Error::last_os_error`]. `EINTR` is not a failure — a profiler
//!    or a `SIGWINCH` is allowed to interrupt a wait — so it reports "nothing
//!    ready" and the caller loops against its own deadline.
//! 3. No `unsafe` escapes the file. Everything above it sees `io::Result`.

// Deliberate, contained, and the second and last of these in df-core.
#![allow(unsafe_code)]

use std::io::{self, Read as _, Write as _};
use std::os::unix::io::AsRawFd;
use std::process::{ChildStderr, ChildStdin, ChildStdout};
use std::time::{Duration, Instant};

/// The raw descriptor of one of a child's pipes, for the calls below. The
/// caller keeps the pipe alive for as long as it uses the number.
fn fd(pipe: &impl AsRawFd) -> i32 {
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

/// What one wait on the child's stdout came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Read {
    /// Bytes were appended.
    Data,
    /// Stdout is closed: the child is gone.
    Eof,
    /// Nothing yet — the wait ran out, or was interrupted, or only stderr
    /// spoke. The caller checks its deadline and asks again.
    Nothing,
}

/// What one wait on the child's stdin came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrote {
    /// This many bytes went (zero means the pipe took nothing: the child is
    /// gone).
    Bytes(usize),
    /// Nothing yet: no room before the wait ran out, or interrupted.
    Nothing,
    /// The pipe is broken: the child exited, and its stderr says why.
    Broken,
}

/// The child's three pipes, and what it has said on stderr.
pub struct Pipes {
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: ChildStderr,
    /// One `read` off stdout.
    scratch: Vec<u8>,
    errbuf: Vec<u8>,
    stderr_cap: usize,
    /// The child's stderr reached EOF. Once it has, it is readable forever, so
    /// it stops being polled — otherwise every wait would return instantly and
    /// the deadline would be enforced by a busy loop.
    stderr_done: bool,
}

impl Pipes {
    /// Take the child's pipes: stdout read `read_size` bytes at a time, stderr
    /// kept up to `stderr_cap` bytes. `label` names the connection in the log.
    ///
    /// Stdin and stderr are made non-blocking, not stdout: see
    /// [`set_nonblocking`] for why stdout stays blocking. Stderr must not
    /// block because it is read *without* a preceding poll saying "readable"
    /// when the connection has died. A failure here is not fatal — a blocking
    /// pipe is still correct, just capable of blocking past its deadline — so
    /// it is logged rather than raised, and this never fails.
    pub fn open(
        stdin: ChildStdin,
        stdout: ChildStdout,
        stderr: ChildStderr,
        read_size: usize,
        stderr_cap: usize,
        label: &str,
    ) -> io::Result<Pipes> {
        for (name, fd) in [("stdin", fd(&stdin)), ("stderr", fd(&stderr))] {
            if let Err(e) = set_nonblocking(fd) {
                log::warn!("vfs {label}: {name} stayed blocking: {e}");
            }
        }
        Ok(Pipes {
            stdin,
            stdout,
            stderr,
            scratch: vec![0; read_size],
            errbuf: Vec::new(),
            stderr_cap,
            stderr_done: false,
        })
    }

    /// Everything the child has said on stderr so far, up to the cap.
    pub fn stderr(&self) -> &[u8] {
        &self.errbuf
    }

    /// Wait up to `left` for stdout, collecting stderr as it arrives, and
    /// append one read's worth to `into`.
    pub fn read(&mut self, into: &mut Vec<u8>, left: Duration) -> io::Result<Read> {
        // A negative fd is ignored by `poll`, which is how a finished
        // stderr stops waking the loop.
        let err_fd = if self.stderr_done {
            -1
        } else {
            fd(&self.stderr)
        };
        let (out_ready, err_ready) = poll_read2(fd(&self.stdout), err_fd, left)?;
        if err_ready {
            self.slurp_stderr();
        }
        if !out_ready {
            return Ok(Read::Nothing);
        }
        match self.stdout.read(&mut self.scratch) {
            // EOF on stdout: the child is gone.
            Ok(0) => Ok(Read::Eof),
            Ok(n) => {
                into.extend_from_slice(&self.scratch[..n]);
                Ok(Read::Data)
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(Read::Nothing)
            }
            Err(e) => Err(e),
        }
    }

    /// Wait up to `left` for room in stdin, and write what fits of `bytes`.
    pub fn write(&mut self, bytes: &[u8], left: Duration) -> io::Result<Wrote> {
        if !poll_write(fd(&self.stdin), left)? {
            return Ok(Wrote::Nothing);
        }
        match self.stdin.write(bytes) {
            Ok(n) => Ok(Wrote::Bytes(n)),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(Wrote::Nothing)
            }
            // A broken pipe means the child exited; its stderr says why.
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(Wrote::Broken),
            Err(e) => Err(e),
        }
    }

    /// Give stderr until `deadline` to deliver the child's parting words after
    /// the connection has died, then stop caring. Called on the EOF and
    /// broken-pipe paths, where the cause of death is racing down the other
    /// pipe.
    pub fn drain_stderr(&mut self, deadline: Instant) {
        while !self.stderr_done {
            let Some(left) = remaining(deadline) else {
                return;
            };
            match poll_read2(fd(&self.stderr), -1, left) {
                Ok((true, _)) => self.slurp_stderr(),
                Ok(_) => return, // the grace period elapsed with nothing there
                Err(_) => return,
            }
        }
    }

    /// Read whatever the child has written to stderr right now, up to the cap.
    fn slurp_stderr(&mut self) {
        let mut buffer = [0u8; 4096];
        match self.stderr.read(&mut buffer) {
            Ok(0) => self.stderr_done = true,
            Ok(n) => {
                let room = self.stderr_cap.saturating_sub(self.errbuf.len());
                if room > 0 {
                    self.errbuf.extend_from_slice(&buffer[..n.min(room)]);
                }
            }
            // Nothing there after all, or a signal. Either way there is nothing
            // to do but carry on; stderr is diagnostic, never load-bearing.
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock) => {}
            Err(_) => self.stderr_done = true,
        }
    }
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
fn poll_read2(stdout: i32, stderr: i32, timeout: Duration) -> io::Result<(bool, bool)> {
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
fn poll_write(fd: i32, timeout: Duration) -> io::Result<bool> {
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
/// Only the child's **stdin** and **stderr** get this. Its stdout stays
/// blocking, because a `read` that follows a `poll` saying "readable" returns
/// immediately anyway, and leaving it blocking keeps the read path one syscall
/// simpler. Stdin is different: `poll` says "writable" when *some* room exists,
/// and a 32 KiB write into 8 KiB of room would block for the rest — so the
/// write path needs partial writes, and partial writes need `O_NONBLOCK`.
fn set_nonblocking(fd: i32) -> io::Result<()> {
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
