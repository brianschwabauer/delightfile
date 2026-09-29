//! What a fresh install ships on Linux: the openers, the rules that pick
//! among them, and the `g` bookmarks — Brian's yazi config, transcribed in
//! [`crate::config`] and read from there unchanged
//! (`plans/other-platforms/05-defaults-and-config.md` §2, §6).

pub use crate::config::{
    DEFAULT_BOOKMARKS as BOOKMARKS, DEFAULT_OPENERS as OPENERS, DEFAULT_RULES as RULES,
};

/// Rows the platform lays over the shipped keymap: none. Linux's keymap is
/// the table in `keymap/defaults.rs` as it stands.
pub const KEYMAP_OVERRIDES: &[(&str, &str, &str, &str)] = &[];
