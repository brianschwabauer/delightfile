//! How a chord is written on Linux: each modifier by its name and a `+`,
//! `Ctrl+Alt+x`, as the help sheet always has (05-defaults-and-config.md §3).

use crate::keymap::Modifier;

/// Each modifier's mark, in the order a chord's label writes them.
pub const LABELS: [(Modifier, &str); 4] = [
    (Modifier::Ctrl, "Ctrl+"),
    (Modifier::Alt, "Alt+"),
    (Modifier::Super, "Super+"),
    (Modifier::Shift, "Shift+"),
];

/// Whether a `super+…` in `keymap.toml` means `ctrl+…`: no, Super is a key
/// of its own here.
pub const SUPER_IS_CTRL: bool = false;

/// The `ctrl` role's name in running text: "Ctrl-click".
pub fn ctrl_name() -> &'static str {
    "Ctrl"
}
