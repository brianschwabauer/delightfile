//! Where `libpdfium.dylib` is looked for on macOS, before the system loader
//! is asked (`crate::preview::doc::pdf`): an explicit path, the one inside
//! the `.app` bundle the release ships it in, and the same per-user place as
//! on Linux.

use std::path::PathBuf;

/// The library's file name on this platform — what the system loader is
/// asked for too.
pub const LIBRARY_NAME: &str = "libpdfium.dylib";

/// Candidate paths for the library, most specific first.
pub fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = std::env::var_os("DF_PDFIUM_LIB") {
        out.push(PathBuf::from(explicit));
    }
    // `delightfile.app/Contents/MacOS/delightfile` → `Contents/Frameworks`.
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
    {
        out.push(exe_dir.join("../Frameworks").join(LIBRARY_NAME));
    }
    if let Some(home) = std::env::var_os("HOME") {
        out.push(
            PathBuf::from(home)
                .join(".local/lib/delightfile")
                .join(LIBRARY_NAME),
        );
    }
    out
}
