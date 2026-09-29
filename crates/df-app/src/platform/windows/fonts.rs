//! Where a Nerd Font is looked for on Windows
//! (`plans/other-platforms/05-defaults-and-config.md` D5.6): the per-user
//! fonts a person installs without an administrator, then the machine's.

use std::path::PathBuf;

/// The directories scanned for a patched face, in order
/// ([`crate::icons::install`]). Each is listed once, not walked.
pub fn dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
    }
    dirs.push(PathBuf::from(r"C:\Windows\Fonts"));
    dirs
}
