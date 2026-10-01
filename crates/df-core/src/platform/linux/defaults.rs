//! What a fresh install ships on Linux: the openers, the rules that pick
//! among them, and the `g` bookmarks — Brian's yazi config, transcribed in
//! [`crate::config`] and read from there unchanged
//! (`plans/other-platforms/05-defaults-and-config.md` §2, §6).

pub use crate::config::{
    DEFAULT_BOOKMARKS as BOOKMARKS, DEFAULT_OPENERS as OPENERS, DEFAULT_RULES as RULES,
};

/// The openers a fresh install ships, one command per row: [`OPENERS`] as
/// it stands (Windows picks among candidates here).
pub fn openers() -> Vec<(&'static str, String, bool, &'static str)> {
    OPENERS
        .iter()
        .map(|(id, command, block, description)| {
            (*id, (*command).to_string(), *block, *description)
        })
        .collect()
}

/// The `g` bookmarks a fresh install ships: [`BOOKMARKS`] as it stands.
pub fn bookmarks() -> Vec<crate::config::Bookmark> {
    BOOKMARKS
        .iter()
        .map(|(key, path, description)| crate::config::Bookmark::row(key, path, description))
        .collect()
}

/// Rows the platform lays over the shipped keymap: none. Linux's keymap is
/// the table in `keymap/defaults.rs` as it stands.
pub const KEYMAP_OVERRIDES: &[(&str, &str, &str, &str)] = &[];
