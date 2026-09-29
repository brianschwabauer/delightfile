//! The parts of running another program that differ by platform: the null
//! device, stopping and continuing a child, what counts as an executable on
//! `PATH` and under which names a tool is looked for.

use std::io;
use std::path::Path;
use std::process::Child;

/// The file that discards what is written to it and reads as empty.
pub const NULL_DEVICE: &str = "/dev/null";

/// Whether `rsync` is a tool this platform has: yes on Unix, where
/// [`crate::sync::rsync::available`] then asks whether it is installed.
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

/// Whether `path` is a file some execute bit is set on.
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The file names a tool called `name` may have on `PATH`: just its name.
pub fn candidates(name: &str) -> Vec<String> {
    vec![name.to_string()]
}
