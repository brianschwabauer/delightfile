//! How a chord is written on Windows: as on Linux, each modifier by its
//! name and a `+` (05-defaults-and-config.md §3).

use crate::keymap::Modifier;

/// Each modifier's mark, in the order a chord's label writes them.
pub const LABELS: [(Modifier, &str); 4] = [
    (Modifier::Ctrl, "Ctrl+"),
    (Modifier::Alt, "Alt+"),
    (Modifier::Super, "Super+"),
    (Modifier::Shift, "Shift+"),
];

/// Whether a `super+…` in `keymap.toml` means `ctrl+…`: no.
pub const SUPER_IS_CTRL: bool = false;

/// The `ctrl` role's name in running text: "Ctrl-click".
pub fn ctrl_name() -> &'static str {
    "Ctrl"
}
