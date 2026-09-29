//! Where a user's files of each kind live on Windows — only the two with an
//! obvious answer until D5.1 (W4.24) fills in `%APPDATA%` and
//! `%LOCALAPPDATA%`. The rest are `None`, which every caller already handles:
//! no config file is read, the state store is session-only, and zoxide's
//! database is not looked for.

use std::path::PathBuf;

/// `%USERPROFILE%`.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Not yet (D5.1).
pub fn config_dir() -> Option<PathBuf> {
    None
}

/// Not yet (D5.1).
pub fn state_dir() -> Option<PathBuf> {
    None
}

/// Not yet (D5.1).
pub fn data_dir() -> Option<PathBuf> {
    None
}

/// Not yet (D5.1).
pub fn cache_dir() -> Option<PathBuf> {
    None
}

/// Windows has no per-session runtime directory.
pub fn runtime_dir() -> Option<PathBuf> {
    None
}

/// `%TEMP%`, as `std` finds it.
pub fn temp_dir() -> Option<PathBuf> {
    Some(std::env::temp_dir())
}
