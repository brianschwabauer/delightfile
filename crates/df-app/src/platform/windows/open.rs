//! Running a snippet on Windows: not yet. There is no `$SHELL -c` to hand a
//! snippet and its paths to; openers there are argv lists and a typed `;`
//! goes through `cmd /C`, which is Phase 4's
//! (`plans/other-platforms/04-windows.md` W4.3). Until then both calls
//! refuse with an `Unsupported` error that says so in the words the window
//! toasts.

use std::io;
use std::path::{Path, PathBuf};

/// The refusal both calls give.
fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Running programs is not available on this platform",
    )
}

/// Refused: no opener runs here yet (W4.3).
pub fn spawn_detached(_snippet: &str, _paths: &[PathBuf], _cwd: &Path) -> io::Result<()> {
    Err(refused())
}

/// Refused: no opener runs here yet (W4.3).
pub fn run_blocking(_snippet: &str, _paths: &[PathBuf], _cwd: &Path) -> io::Result<i32> {
    Err(refused())
}
