//! Where a user's files of each kind live on macOS: the XDG rules with `$HOME`
//! fallbacks, as on Linux — not `~/Library` — because that is what yazi does
//! on a Mac (its `Xdg` has no macOS branch: `~/.config/yazi`,
//! `~/.local/state/yazi`, and its thumbnails in `$TMPDIR/yazi-<uid>`), so a
//! person coming from yazi finds delightfile's files beside its own
//! (`plans/other-platforms/05-defaults-and-config.md` §1).
//!
//! The one answer that differs is the runtime directory. macOS sets no
//! `$XDG_RUNTIME_DIR`; what it has instead is `$TMPDIR`, a `/var/folders/…/T/`
//! of each user's own that nobody else can enter.

use std::path::PathBuf;

pub use crate::platform::unix::dirs::{
    cache_dir, config_dir, data_dir, home, state_dir, temp_dir, yazi_config_dir,
};

/// `$TMPDIR`, the per-user temporary directory, standing in for the
/// `$XDG_RUNTIME_DIR` macOS does not have.
pub fn runtime_dir() -> Option<PathBuf> {
    Some(std::env::temp_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runtime directory is the user's own temporary one, and the rest
    /// are the XDG rules Linux keeps.
    #[test]
    fn the_runtime_directory_is_the_users_tmpdir() {
        assert_eq!(runtime_dir(), Some(std::env::temp_dir()));
        let home = home().expect("a runner has a $HOME");
        if std::env::var_os("XDG_CONFIG_HOME").is_none() {
            assert_eq!(config_dir(), Some(home.join(".config")));
        }
        if std::env::var_os("XDG_STATE_HOME").is_none() {
            assert_eq!(state_dir(), Some(home.join(".local").join("state")));
        }
    }
}
