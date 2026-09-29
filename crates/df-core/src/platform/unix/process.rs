//! The parts of running another program that differ by platform: the null
//! device, stopping and continuing a child, the number a finished one exited
//! with, what counts as an executable on `PATH` and under which names a tool
//! is looked for.

use std::io;
use std::path::Path;
use std::process::{Child, ExitStatus};

/// The file that discards what is written to it and reads as empty.
pub const NULL_DEVICE: &str = "/dev/null";

/// Whether `rsync` is a tool this platform has: yes on Unix, where
/// [`crate::sync::rsync::available`] then asks whether it is installed and
/// new enough.
pub const HAS_RSYNC: bool = true;

/// Stop `child` (`SIGSTOP`): how a pause reaches a process that is not ours
/// to checkpoint.
pub fn pause(child: &Child) -> io::Result<()> {
    signal(child, libc::SIGSTOP)
}

/// Let a stopped `child` run again (`SIGCONT`).
pub fn resume(child: &Child) -> io::Result<()> {
    signal(child, libc::SIGCONT)
}

/// Send `child` a signal. The caller holds the `Child`, so the pid is one this
/// process spawned and has not yet reaped.
fn signal(child: &Child, signal: libc::c_int) -> io::Result<()> {
    let pid = libc::pid_t::try_from(child.id())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // One syscall on a pid this process spawned and has not yet reaped.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::kill(pid, signal) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Ask `child` to exit with `SIGTERM`, the signal rclone cleans up on.
///
/// A child that has already been reaped is left alone and is `Ok`: once `std`
/// has collected its status the pid is free for the kernel to hand to another
/// process, and a signal sent to the number would land on a stranger. One not
/// yet reaped is still ours — a zombie keeps its pid until it is waited for —
/// so the check-then-signal here has no window.
#[allow(unsafe_code)]
pub fn terminate(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    let pid = libc::pid_t::try_from(child.id())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `kill` takes two integers and touches no memory of ours. `pid`
    // is a child this `Child` has not reaped (checked above), so it names our
    // own process.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The number a finished child exited with, as a shell's `$?` says it.
pub fn exit_code(status: &ExitStatus) -> i32 {
    // A signalled child has no code; 128 + signal is what every shell reports
    // for one, so the toast says the number the user would see in `$?`.
    status.code().unwrap_or_else(|| {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    })
}

/// Whether `path` is a file some execute bit is set on.
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The file names a tool called `name` may have on `PATH`: just its name.
pub fn candidates(name: &str) -> Vec<String> {
    vec![name.to_string()]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use std::process::Command;

    /// What `$?` would say: the code when the child exited, 128 + the signal
    /// when one ended it.
    #[test]
    fn an_exit_code_is_the_one_a_shell_would_report() {
        let exited = Command::new("sh").args(["-c", "exit 3"]).status().unwrap();
        assert_eq!(exit_code(&exited), 3);
        let killed = Command::new("sh")
            .args(["-c", "kill -TERM $$"])
            .status()
            .unwrap();
        assert_eq!(exit_code(&killed), 128 + libc::SIGTERM);
    }
}
