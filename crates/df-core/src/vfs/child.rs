//! The rclone daemon's life, tied to ours. **The vfs's second island of
//! `unsafe`**, beside `poll`, and written to the same three rules.
//!
//! An `ssh` child dies with delightfile for free: when delightfile goes, its
//! end of the pipes closes, and `ssh` exits on the EOF at its stdin. `rclone
//! rcd` has no stdin to watch — it serves a socket — so if delightfile dies
//! without running its destructors (a panic with `panic = "abort"`, `SIGKILL`,
//! the OOM killer, a compositor that kills the client), the daemon lives on,
//! holding a socket that answers anyone who can reach the directory it is in
//! and a remote the user thought was closed. Two syscalls close that hole:
//!
//! - **`prctl(PR_SET_PDEATHSIG, SIGTERM)`**, run in the child between `fork`
//!   and `exec`, asks the kernel to send the daemon `SIGTERM` when its parent
//!   dies — however it dies, destructors or none. Strictly, when the parent
//!   *thread* that spawned it exits, which is exactly the right one here: the
//!   vfs worker thread that spawns a service's daemon is the thread that owns
//!   it, and when that thread is gone nothing can talk to the daemon anyway.
//! - **`getppid()`**, straight after, closes the race the first call leaves
//!   open: a parent that died *before* the `prctl` landed sends no signal,
//!   because it is already dead. The child compares its parent with the pid
//!   captured before the spawn and, if the parent has changed (the child has
//!   been re-parented to init or a subreaper), exits before `exec` rather than
//!   start a daemon nobody owns.
//!
//! The rules, unchanged from `fs::inotify` and `poll`:
//!
//! 1. Nothing here **owns** a process. It configures a `Command` that the
//!    caller spawns and owns.
//! 2. Every syscall's return is checked and turned into an
//!    [`std::io::Error::last_os_error`].
//! 3. No `unsafe` escapes the file. Everything above it sees safe functions.
//!
//! The closure given to `pre_exec` runs in the forked child of a threaded
//! process, where only async-signal-safe work is allowed: it makes two
//! syscalls and builds errors that do not allocate, and nothing else.

// Deliberate, contained, and documented above; the vfs's other island is
// `poll`.
#![allow(unsafe_code)]

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Arrange for the child `command` is about to spawn to receive `SIGTERM` when
/// the calling thread exits, and to not start at all if this process is
/// already gone by the time it runs.
///
/// Call it on the thread that will own the child for its whole life; see the
/// module note on why that is the thread, not the process.
pub(super) fn tie_to_this_thread(command: &mut Command) {
    // Captured before the fork: in the child, `getppid` is compared against
    // it, and a mismatch means the process that spawned it has already died.
    let parent = libc::pid_t::try_from(std::process::id()).unwrap_or(libc::pid_t::MAX);
    // SAFETY: the closure runs between `fork` and `exec` in the child. It
    // makes two async-signal-safe syscalls, touches no lock and no allocator
    // (`last_os_error` reads errno; `from_raw_os_error` stores an integer),
    // and borrows nothing from the parent's memory but the copied `parent`.
    unsafe {
        command.pre_exec(move || {
            // The signal number travels as an `unsigned long`, which is what
            // the kernel reads for this option.
            if libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGTERM as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                // The parent died before the line above could tell the kernel
                // to warn us. Returning an error makes `std` end the child
                // without exec'ing rclone.
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}
