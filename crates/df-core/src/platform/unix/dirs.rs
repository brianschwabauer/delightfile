//! Where a user's files of each kind live, by the XDG base-directory rules with
//! `$HOME` fallbacks — Linux's convention, and what yazi and zoxide use on
//! macOS too, so this phase gives macOS the same answers (D5.1 decides whether
//! macOS moves to `~/Library`).
//!
//! Each function is the rule one of the callers used inline before the seam,
//! so the answers are exactly the ones they gave: a set-but-empty variable
//! counts as unset, and a relative one is taken as it is (the trash, which the
//! spec says must ignore a relative `$XDG_DATA_HOME`, keeps its own rule).
//! Every function reads the environment when called; nothing is cached, so a
//! test that sets a variable sees it.

use std::ffi::OsString;
use std::path::PathBuf;

/// `$name`, unless it is unset or empty.
fn set(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

/// `$HOME`, as it is set (an empty one included).
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `$XDG_CONFIG_HOME`, else `$HOME/.config`.
pub fn config_dir() -> Option<PathBuf> {
    set("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".config")))
}

/// `$XDG_STATE_HOME`, else `$HOME/.local/state` — `None` when `$HOME` is unset
/// or empty too, which leaves the state store session-only.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = set("XDG_STATE_HOME") {
        return Some(PathBuf::from(dir));
    }
    let home = set("HOME")?;
    Some(PathBuf::from(home).join(".local").join("state"))
}

/// `$XDG_DATA_HOME`, else `$HOME/.local/share`.
pub fn data_dir() -> Option<PathBuf> {
    match set("XDG_DATA_HOME") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(home()?.join(".local/share")),
    }
}

/// `$XDG_CACHE_HOME`, else `$HOME/.cache`.
pub fn cache_dir() -> Option<PathBuf> {
    set("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".cache")))
}

/// `$XDG_RUNTIME_DIR`, as it is set. The callers differ on what makes one
/// usable (gvfs needs it absolute, the rclone socket only non-empty), so each
/// applies its own test; with none set, each has its own fallback.
pub fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
}

/// The system's temporary directory (`$TMPDIR`, else `/tmp`).
pub fn temp_dir() -> Option<PathBuf> {
    Some(std::env::temp_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state file's pure rule (kept for tests and df-app's portal) and the
    /// directory the store actually uses cannot drift apart.
    #[test]
    fn the_state_directory_is_the_state_files_own_rule() {
        let rule = crate::state::state_path_from(
            std::env::var_os("XDG_STATE_HOME"),
            std::env::var_os("HOME"),
        );
        assert_eq!(
            state_dir().map(|dir| dir.join("delightfile").join("state")),
            rule
        );
    }
}
