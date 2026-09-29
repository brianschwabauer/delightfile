//! Where `libpdfium.so` is looked for on Linux, before the system loader is
//! asked (`crate::preview::doc::pdf`, whose header gives the order and why).

use std::path::{Path, PathBuf};

/// The library's file name on this platform — what the system loader is
/// asked for too.
pub const LIBRARY_NAME: &str = "libpdfium.so";

/// Candidate paths for the dynamic library, most specific first.
pub fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = std::env::var_os("DF_PDFIUM_LIB") {
        out.push(PathBuf::from(explicit));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        out.push(home.join(".local/lib/delightfile").join(LIBRARY_NAME));
        // The sibling program's copy. Sharing it is the point of pinning the
        // same ABI: there is one 7 MB library on this machine and neither
        // program should be the reason there are two.
        out.push(home.join(".local/lib/delightviewer").join(LIBRARY_NAME));
    }
    out.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(LIBRARY_NAME),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The per-user copies are both looked for: delightfile's own, and the
    /// sibling program's, which is shared on purpose.
    #[test]
    fn the_user_and_the_sibling_copies_are_both_looked_for() {
        let paths = candidates();
        if std::env::var_os("HOME").is_some() {
            assert!(
                paths
                    .iter()
                    .any(|p| p.ends_with(".local/lib/delightfile/libpdfium.so")),
                "{paths:?}"
            );
            assert!(
                paths
                    .iter()
                    .any(|p| p.ends_with(".local/lib/delightviewer/libpdfium.so")),
                "the sibling's copy is shared on purpose: {paths:?}"
            );
        }
    }
}
