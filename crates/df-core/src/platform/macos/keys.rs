//! How a chord is written on a Mac: the key caps' symbols, run together
//! in Apple's order — Control, Option, Shift, Command — so `ctrl+shift+left`
//! reads `⇧⌘←` (05-defaults-and-config.md §3, M2.21).
//!
//! The `ctrl` role is Command: df-app reads Command and Control both as
//! `ctrl` (M2.20), and Command is the one a Mac's hand goes to, so that is
//! the mark it wears. `⌃` is the Super role's, which nothing makes here: the
//! same fold turns a Super into a `ctrl`, and [`SUPER_IS_CTRL`] does it to a
//! `super+…` written in `keymap.toml`.

use crate::keymap::Modifier;

/// Each modifier's mark, in the order a chord's label writes them.
pub const LABELS: [(Modifier, &str); 4] = [
    (Modifier::Super, "⌃"),
    (Modifier::Alt, "⌥"),
    (Modifier::Shift, "⇧"),
    (Modifier::Ctrl, "⌘"),
];

/// Whether a `super+…` in `keymap.toml` means `ctrl+…`: yes, Super is
/// Command, and Command is `ctrl` here. The keymap says so in a warning.
pub const SUPER_IS_CTRL: bool = true;

/// The `ctrl` role's name in running text: "⌘-click".
pub fn ctrl_name() -> &'static str {
    "⌘"
}

#[cfg(test)]
mod tests {
    use crate::keymap::{parse_chord, parse_sequence, Command, Context, Registry};

    /// A chord in the Mac's words: the marks run together in Apple's order,
    /// the capital still the Shift.
    #[test]
    fn a_chord_is_written_with_the_key_caps_symbols() {
        let label = |text: &str| parse_chord(text).expect("chord").label();
        assert_eq!(label("alt+left"), "⌥←");
        assert_eq!(label("ctrl+p"), "⌘p");
        assert_eq!(label("ctrl+shift+z"), "⌘Z");
        assert_eq!(label("ctrl+shift+left"), "⇧⌘←");
        assert_eq!(label("ctrl+alt+shift+tab"), "⌥⇧⌘Tab");
        assert_eq!(label("G"), "G");
    }

    /// `super+x` in a Mac's `keymap.toml` binds Cmd+x, and says why.
    #[test]
    fn a_super_binding_is_a_cmd_binding_with_a_warning() {
        let mut km = Registry::defaults();
        let warnings = km.apply_overrides(
            "[files]\n\"super+k\" = \"quit\"\n",
            std::path::Path::new("keymap.toml"),
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0]
                .message
                .contains("super is Cmd, which is Ctrl on macOS"),
            "{warnings:?}"
        );
        let cmd_k = parse_sequence("ctrl+k").expect("chord")[0];
        assert_eq!(km.lookup(Context::Files, cmd_k), Some(Command::Quit));
    }
}
