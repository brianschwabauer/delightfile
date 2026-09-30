//! The parts of running another program that differ by platform, as Windows
//! has them: `NUL`, no stopping a child, an exit code that is always there,
//! executables known by their extension, tools that carry `.exe`, and a tool
//! run with no console window of its own.
//!
//! delightfile is a window, not a console program, so every console tool it
//! starts — `git`, `7z`, `tar`, `ssh`, `rclone` — would get a console of its
//! own, and its window would flash up over the app for as long as the tool
//! runs. [`quiet`] starts one with `CREATE_NO_WINDOW` instead, which is what
//! a headless tool with piped or null stdio wants. It is never for an opener:
//! a `cmd` or a `wt` that a person asked for needs the console it gets.

use std::io;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};

use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

pub use crate::platform::stub::process::*;

/// The file that discards what is written to it and reads as empty.
pub const NULL_DEVICE: &str = "NUL";

/// No `rsync` on Windows, ever: it is not a Windows tool, and its `host:path`
/// syntax collides with drive letters. The remote sync is not offered.
pub const HAS_RSYNC: bool = false;

/// Nothing in place of the bare "rsync": no rsync is ever asked for here.
pub const RSYNC_HINT: &str = "";

/// Refused: Windows has no `SIGSTOP`, and suspending another process's
/// threads one by one is not a pause worth offering (W4.2).
pub fn pause(_child: &Child) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Refused, as [`pause`].
pub fn resume(_child: &Child) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Stop `child`: `TerminateProcess`, since Windows has no gentler signal a
/// console-less child would see. A child already reaped is left alone.
pub fn terminate(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    child.kill()
}

/// What [`quiet`] sets: `CREATE_NO_WINDOW`, and no other creation flag.
const QUIET: u32 = CREATE_NO_WINDOW;

/// Run the tool `command` starts without a console window. The only creation
/// flag df-core sets, so it overwrites none. Returns `command` for chaining.
pub fn quiet(command: &mut Command) -> &mut Command {
    command.creation_flags(QUIET)
}

/// The number a finished child exited with. A Windows process always has
/// one — there are no signals to end it without — so the 1 is never reached;
/// it stands for a failure all the same, never for a success.
pub fn exit_code(status: &ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// Whether `path` is a file whose extension is one `PATHEXT` names
/// (`.COM;.EXE;.BAT;.CMD` when it is unset), in any case: Windows has no
/// execute bit, and the extension is what decides whether a name runs.
pub fn is_executable(path: &Path) -> bool {
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    pathext
        .split(';')
        .filter_map(|known| known.strip_prefix('.'))
        .any(|known| known.eq_ignore_ascii_case(ext))
}

/// The file names a tool called `name` may have on `PATH`: `name.exe` first,
/// then the name as given. `bsdtar` is also looked for as `tar`, because
/// Windows 10 and later ship libarchive's bsdtar as `tar.exe` (where the GNU
/// tar flags a Linux `tar` would need never come up).
pub fn candidates(name: &str) -> Vec<String> {
    let mut names = vec![format!("{name}.exe"), name.to_string()];
    if name == "bsdtar" {
        names.push("tar.exe".to_string());
        names.push("tar".to_string());
    }
    names
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    /// `std` cannot read a command's creation flags back, and a console that
    /// never appears cannot be seen headlessly; so the flag is checked by
    /// value, and a quiet tool is run to show its pipes still carry its
    /// output.
    #[test]
    fn a_quiet_tool_has_no_window_and_still_talks() {
        assert_eq!(QUIET, 0x0800_0000, "CREATE_NO_WINDOW");
        let out = quiet(&mut Command::new("cmd"))
            .args(["/C", "echo", "quiet"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "quiet");
    }

    #[test]
    fn a_tool_is_looked_for_with_its_extension_first() {
        assert_eq!(candidates("7z"), ["7z.exe", "7z"]);
        assert_eq!(
            candidates("bsdtar"),
            ["bsdtar.exe", "bsdtar", "tar.exe", "tar"]
        );
    }
}
