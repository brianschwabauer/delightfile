//! Where `pdfium.dll` is looked for on Windows, before the system loader is
//! asked (`crate::preview::doc::pdf`): an explicit path, then beside the
//! executable, which is where the release zip puts it.

use std::path::PathBuf;

/// The library's file name on this platform — what the system loader is
/// asked for too.
pub const LIBRARY_NAME: &str = "pdfium.dll";

/// Candidate paths for the library, most specific first.
pub fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = std::env::var_os("DF_PDFIUM_LIB") {
        out.push(PathBuf::from(explicit));
    }
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
    {
        out.push(exe_dir.join(LIBRARY_NAME));
    }
    out
}
