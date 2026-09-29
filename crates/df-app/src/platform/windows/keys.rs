//! The modifiers a keystroke is read with, on Windows: each one as itself,
//! as on Linux (`plans/other-platforms/05-defaults-and-config.md` §3: the
//! Windows keymap is Linux's).

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
