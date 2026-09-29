//! The parts of running another program that differ by platform, as Windows
//! has them: `NUL`, no stopping a child, executables known by their extension,
//! and tools that carry `.exe`.

use std::io;
use std::path::Path;
use std::process::Child;

pub use crate::platform::stub::process::*;

/// The file that discards what is written to it and reads as empty.
pub const NULL_DEVICE: &str = "NUL";

/// No `rsync` on Windows, ever: it is not a Windows tool, and its `host:path`
/// syntax collides with drive letters. The remote sync is not offered.
pub const HAS_RSYNC: bool = false;

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
    use super::*;

    #[test]
    fn a_tool_is_looked_for_with_its_extension_first() {
        assert_eq!(candidates("7z"), ["7z.exe", "7z"]);
        assert_eq!(
            candidates("bsdtar"),
            ["bsdtar.exe", "bsdtar", "tar.exe", "tar"]
        );
    }
}
