//! The modifiers a keystroke is read with, on Linux: each one as itself.
//! Ctrl is Ctrl and Super is Super, as the keymap spells them; moved here
//! unchanged from `keys::chord_from` so that macOS can read Command as Ctrl
//! without the caller knowing.

use df_core::keymap::Mods;
use winit::keyboard::ModifiersState;

/// The modifiers held, as the keymap names them.
pub fn mods(state: ModifiersState) -> Mods {
    Mods {
        ctrl: state.control_key(),
        alt: state.alt_key(),
        shift: state.shift_key(),
        super_key: state.super_key(),
    }
}

/// Whether a press is a character composed with modifiers that are not
/// modifiers here: never on Linux, where AltGr is a level of its own and
/// never arrives as Ctrl+Alt.
pub fn composed(_state: ModifiersState, _text: Option<&str>) -> bool {
    false
}
