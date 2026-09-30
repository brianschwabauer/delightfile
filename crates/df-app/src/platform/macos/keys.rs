//! The modifiers a keystroke is read with, on macOS: **Command is Ctrl.**
//!
//! The keymap is written once, in `ctrl+…` chords, and on a Mac the key a
//! hand goes to for those is Command (`plans/other-platforms/02-macos.md`,
//! "Cmd is Ctrl for chord matching"). So Command and Control both read as
//! `ctrl`, the same keymap file works on every machine, and there is no Super
//! left over: a `super+…` binding would be a second name for the one key.
//! Option arrives as Alt because the window reads it as one
//! ([`super::window`]).

use df_core::keymap::Mods;
use winit::keyboard::ModifiersState;

/// The modifiers held, as the keymap names them: Command or Control is
/// `ctrl`, and `super_key` is never set.
pub fn mods(state: ModifiersState) -> Mods {
    Mods {
        ctrl: state.control_key() || state.super_key(),
        alt: state.alt_key(),
        shift: state.shift_key(),
        super_key: false,
    }
}

/// Whether a press is a character composed with modifiers that are not
/// modifiers here: never on macOS, where Option is read as Alt and composes
/// nothing ([`super::window`]).
pub fn composed(_state: ModifiersState, _text: Option<&str>) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_and_control_are_one_modifier() {
        let command = mods(ModifiersState::SUPER);
        let control = mods(ModifiersState::CONTROL);
        assert_eq!(command, control);
        assert!(command.ctrl);
        assert!(!command.super_key);
        let both = mods(ModifiersState::SUPER | ModifiersState::CONTROL);
        assert_eq!(both, control, "holding both is still just ctrl");
    }

    #[test]
    fn option_and_shift_pass_through() {
        let held = mods(ModifiersState::ALT | ModifiersState::SHIFT);
        assert!(held.alt && held.shift);
        assert!(!held.ctrl && !held.super_key);
        assert_eq!(mods(ModifiersState::empty()), Mods::NONE);
    }
}
