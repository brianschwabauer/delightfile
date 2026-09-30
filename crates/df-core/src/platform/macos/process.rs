//! macOS's process parts: the ones shared with Linux (the null device, pause
//! and resume, `terminate`), what a Mac is told when its `rsync` is too old,
//! and the rclone daemon's life tied to delightfile's.
//!
//! Linux ties a daemon to its owner with `PR_SET_PDEATHSIG`, a line of
//! kernel state set between `fork` and `exec` (`platform/linux/process.rs`
//! says why the daemon needs it: it serves a socket and has no stdin to see
//! delightfile go). macOS has no such signal. What it has is a kqueue filter,
//! `EVFILT_PROC` with `NOTE_EXIT`, that tells a process when another one
//! exits — and somebody has to be waiting on it when delightfile dies,
//! however it dies, which nothing inside delightfile can be.
//!
//! So [`tie_to_this_thread`] leaves a **watcher** behind: between `fork` and
//! `exec`, the daemon-to-be forks once more, and that second child never
//! execs. It closes every descriptor it inherited (it holds none of the
//! daemon's pipes, none of delightfile's files, and not the pipe `std` uses to
//! learn that the `exec` worked, which would otherwise keep `spawn` waiting
//! on it), makes a kqueue, and sleeps in `kevent` on two exits: delightfile's
//! and the daemon's. If delightfile goes first it sends the daemon `SIGTERM`,
//! the signal Linux's arrangement sends and the one rclone cleans up on; if
//! the daemon goes first there is nothing left to do. Either way it exits,
//! and launchd, which inherits it, reaps it. In the process list it is a
//! second `delightfile`, a child of `rclone`, for as long as the daemon runs.
//!
//! A thread's exit is not an event kqueue reports, so on macOS the daemon is
//! tied to delightfile's **process**, not to the thread that spawned it. The
//! vfs worker that owns a daemon lives as long as the process does, and a
//! daemon it has finished with is stopped by its owner with [`terminate`].
//!
//! The rules of the other islands of `unsafe` hold, and one more: the
//! closure given to `pre_exec` and the watcher run in the child of a process
//! with threads, where only async-signal-safe calls are allowed. Between them
//! they make `getppid`, `getpid`, `fork`, `close`, `kqueue`, `kevent`,
//! `kill` and `_exit`, touch no lock and no allocator, and build no error
//! that allocates.

#![allow(unsafe_code)]

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

pub use crate::platform::unix::process::*;

/// What a refusal says is needed, in place of the bare "rsync", when
/// [`crate::sync::rsync::available`] says no. A Mac always has an `rsync`,
/// and it is always too old ([`crate::sync::rsync::MIN_VERSION`]): Apple
/// ships Samba's 2.6.9 or openrsync, so the one to install is Homebrew's.
pub const RSYNC_HINT: &str = "rsync 3.1 or newer — `brew install rsync`";

/// Arrange for the child `command` is about to spawn to receive `SIGTERM`
/// when this process exits, however it exits, and to not start at all if this
/// process is already gone by the time it runs (see the module note for the
/// watcher that does it, and why it is the process and not the thread).
pub fn tie_to_this_thread(command: &mut Command) {
    // Captured before the fork, as on Linux: in the child, `getppid` is
    // compared against it, and a mismatch means the spawner has already died.
    let parent = libc::pid_t::try_from(std::process::id()).unwrap_or(libc::pid_t::MAX);
    // SAFETY: the closure runs between `fork` and `exec` in the child. It
    // makes three async-signal-safe calls and builds errors that store an
    // integer; the watcher it forks makes only the calls listed in the module
    // note and never returns into the child's Rust.
    unsafe {
        command.pre_exec(move || {
            if libc::getppid() != parent {
                // Returning an error makes `std` end the child without
                // exec'ing the daemon.
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            let daemon = libc::getpid();
            match libc::fork() {
                -1 => Err(io::Error::last_os_error()),
                0 => watch(parent, daemon),
                _ => Ok(()),
            }
        });
    }
}

/// A `kevent` asking for `pid`'s exit.
fn exit_of(pid: libc::pid_t) -> libc::kevent {
    libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// The watcher: wait for `parent` or `daemon` to exit, and if it is `parent`,
/// stop `daemon`. Runs in a process of its own and ends it.
///
/// # Safety
///
/// Only in the child of a `fork`, which it ends with `_exit`: it closes every
/// descriptor the process has.
unsafe fn watch(parent: libc::pid_t, daemon: libc::pid_t) -> ! {
    // SAFETY (the whole body): plain syscalls on integers and on two local
    // `kevent`s; the descriptors closed are this process's own, which nothing
    // in it will use again.
    unsafe {
        for fd in 0..libc::getdtablesize() {
            libc::close(fd);
        }
        let kq = libc::kqueue();
        if kq < 0 {
            libc::_exit(1);
        }
        for pid in [parent, daemon] {
            let change = exit_of(pid);
            if libc::kevent(kq, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) < 0 {
                // `ESRCH`: gone before it could be watched. The daemon is
                // stopped only if it is delightfile that has gone.
                if pid == parent {
                    libc::kill(daemon, libc::SIGTERM);
                }
                libc::_exit(0);
            }
        }
        let mut event = exit_of(0);
        loop {
            let n = libc::kevent(kq, std::ptr::null(), 0, &mut event, 1, std::ptr::null());
            if n < 0 {
                if *libc::__error() == libc::EINTR {
                    continue;
                }
                libc::_exit(1);
            }
            if n == 1 {
                if event.ident == parent as libc::uintptr_t {
                    libc::kill(daemon, libc::SIGTERM);
                }
                libc::_exit(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    /// Whether `pid` is a process this user can see.
    fn alive(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 checks for the process and delivers nothing.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    fn gone_within(pid: libc::pid_t, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        !alive(pid)
    }

    /// The children of `pid`, by `pgrep`.
    fn children_of(pid: libc::pid_t) -> Vec<libc::pid_t> {
        let out = Command::new("pgrep")
            .args(["-P", &pid.to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }

    const HELPER: &str = "DF_TIE_HELPER";

    /// Not a test on its own: run by the one below as a separate process (the
    /// parent that gets killed), it spawns a tied `sleep`, says its pid, and
    /// waits to be killed. Run by the harness, it does nothing.
    #[test]
    fn tied_parent_helper() {
        if std::env::var_os(HELPER).is_none() {
            return;
        }
        let mut command = Command::new("sleep");
        command.arg("60").stdin(Stdio::null());
        tie_to_this_thread(&mut command);
        let mut child = command.spawn().unwrap();
        println!("daemon {}", child.id());
        std::thread::sleep(Duration::from_secs(60));
        // Reached only if nobody killed this process as they said they would.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// **A parent killed with `SIGKILL` takes its daemon with it**: nothing in
    /// the parent runs, and the watcher it left sends the daemon `SIGTERM`
    /// within a second; the watcher is gone as well.
    #[test]
    fn a_daemon_goes_when_its_parent_is_killed() {
        let mut parent = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::macos::process::tests::tied_parent_helper",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HELPER, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        // The harness prints `test <name> ... ` and then runs the test, so
        // the helper's line lands after that on the same line.
        let mut lines = BufReader::new(parent.stdout.take().unwrap()).lines();
        let daemon: libc::pid_t = loop {
            let line = lines.next().unwrap().unwrap();
            if let Some((_, pid)) = line.rsplit_once("daemon ") {
                break pid.trim().parse().unwrap();
            }
        };
        assert!(alive(daemon), "the daemon runs while its parent does");
        std::thread::sleep(Duration::from_millis(100));
        let watchers = children_of(daemon);
        assert_eq!(watchers.len(), 1, "one watcher: {watchers:?}");

        parent.kill().unwrap();
        parent.wait().unwrap();
        assert!(
            gone_within(daemon, Duration::from_secs(1)),
            "the daemon outlived its parent"
        );
        assert!(gone_within(watchers[0], Duration::from_secs(1)));
    }

    /// A daemon that ends on its own ends its watcher too, and the parent
    /// that spawned it is left alone.
    #[test]
    fn a_daemon_that_exits_takes_its_watcher_with_it() {
        let mut command = Command::new("sleep");
        command.arg("0.5").stdin(Stdio::null());
        tie_to_this_thread(&mut command);
        let mut child = command.spawn().unwrap();
        let daemon = libc::pid_t::try_from(child.id()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let watchers = children_of(daemon);
        assert_eq!(watchers.len(), 1, "one watcher: {watchers:?}");
        assert!(child.wait().unwrap().success(), "not killed");
        assert!(gone_within(watchers[0], Duration::from_secs(1)));
    }
}
