//! What a fresh install ships on Windows: Linux's tables from
//! [`crate::config`] until W4.3 gives Windows its own argv openers
//! (`plans/other-platforms/05-defaults-and-config.md` §2.2), which need the
//! argument splitter and `first_available` that phase brings. Nothing runs an
//! opener here yet (`open::spawn_detached` is refused), so these only fill
//! the `O` picker, as they did before the tables were per platform.

pub use crate::config::{
    DEFAULT_BOOKMARKS as BOOKMARKS, DEFAULT_OPENERS as OPENERS, DEFAULT_RULES as RULES,
};

/// Rows the platform lays over the shipped keymap: none, since Windows'
/// keymap is Linux's (05-defaults-and-config.md §3).
pub const KEYMAP_OVERRIDES: &[(&str, &str, &str, &str)] = &[];
