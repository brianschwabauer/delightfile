//! What a fresh install ships on Windows.
//!
//! The openers and rules are Linux's, from [`crate::config`], until W4.3
//! gives Windows its own argv openers
//! (`plans/other-platforms/05-defaults-and-config.md` §2.2), which need the
//! argument splitter and `first_available` that phase brings. Nothing runs an
//! opener here yet (`open::spawn_detached` is refused), so these only fill
//! the `O` picker, as they did before the tables were per platform.
//!
//! The bookmarks are §6's Windows column, which needs nothing from W4.3:
//! Brian's server mounts and hosts are no place on a Windows machine.

pub use crate::config::{DEFAULT_OPENERS as OPENERS, DEFAULT_RULES as RULES};

/// The `g` chord's bookmarks: key, path, description, in which-key order.
///
/// `g c` is the folder `%APPDATA%` names by default, written from `~`
/// because a bookmark expands `~` and nothing else. Desktop and Documents
/// are on the letters a Mac gives them, so `g D` and `g o` mean one place on
/// both.
pub const BOOKMARKS: &[(&str, &str, &str)] = &[
    ("h", "~", "Go home"),
    ("c", "~/AppData/Roaming", "Go to %APPDATA%"),
    ("d", "~/Downloads", "Go to ~/Downloads"),
    ("w", "~/Work", "Go to ~/Work"),
    ("D", "~/Desktop", "Go to ~/Desktop"),
    ("o", "~/Documents", "Go to ~/Documents"),
];

/// Rows the platform lays over the shipped keymap: none, since Windows'
/// keymap is Linux's (05-defaults-and-config.md §3).
pub const KEYMAP_OVERRIDES: &[(&str, &str, &str, &str)] = &[];

#[cfg(test)]
mod tests {
    use crate::config::Config;

    /// A fresh install on Windows goes where §6's Windows column says, the
    /// server mounts Linux ships left out.
    #[test]
    fn default_bookmarks_are_the_goto_table() {
        let c = Config::default();
        let pairs: Vec<(&str, &str)> = c
            .goto
            .iter()
            .map(|b| (b.key.as_str(), b.path.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("h", "~"),
                ("c", "~/AppData/Roaming"),
                ("d", "~/Downloads"),
                ("w", "~/Work"),
                ("D", "~/Desktop"),
                ("o", "~/Documents"),
            ]
        );
    }
}
