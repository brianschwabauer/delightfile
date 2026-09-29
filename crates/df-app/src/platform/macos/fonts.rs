//! Where a Nerd Font is looked for on macOS
//! (`plans/other-platforms/05-defaults-and-config.md` D5.6): the user's own
//! fonts, the machine's, and the system's two. The Homebrew cask
//! (`font-symbols-only-nerd-font`) installs into the first.

use std::path::PathBuf;

/// The directories scanned for a patched face, in order
/// ([`crate::icons::install`]). Each is listed once, not walked.
pub fn dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Library/Fonts"));
    }
    dirs.extend(
        [
            "/Library/Fonts",
            "/System/Library/Fonts",
            "/System/Library/Fonts/Supplemental",
        ]
        .map(PathBuf::from),
    );
    dirs
}
