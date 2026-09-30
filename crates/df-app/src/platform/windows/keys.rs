//! The modifiers a keystroke is read with, on Windows: each one as itself,
//! as on Linux (`plans/other-platforms/05-defaults-and-config.md` §3: the
//! Windows keymap is Linux's), with one exception, AltGr.
//!
//! **AltGr is Ctrl+Alt on Windows.** The system has no AltGr modifier: on a
//! layout that has the key (German, French, Polish, …) pressing it holds a
//! synthetic left Ctrl and the right Alt, and so does holding left Ctrl and
//! left Alt together, which is how a person without the key types `@` or `€`
//! there. winit 0.30.13 already leaves Ctrl and Alt out of the modifiers when
//! the right Alt is down on such a layout, but not for the two-key spelling,
//! which arrives as `ctrl+alt+q` with `@` for its text. A press whose
//! modifiers are exactly that pair and that types a printable character is
//! therefore text ([`composed`]) — the key the person meant, typed into
//! whatever is open — and never a Ctrl+Alt binding. The default keymap has
//! no Ctrl+Alt binding, and on a layout without AltGr a Ctrl+Alt letter types
//! nothing, so what a `keymap.toml` binds there still fires
//! (`plans/other-platforms/04-windows.md` W4.25).

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

/// Whether a press is a character AltGr composed: Ctrl and Alt both held,
/// and printable `text` (the caller has already dropped control characters).
pub fn composed(state: ModifiersState, text: Option<&str>) -> bool {
    state.control_key() && state.alt_key() && text.is_some_and(|text| !text.is_empty())
}
