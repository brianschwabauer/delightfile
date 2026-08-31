//! winit keystrokes → df-core [`Chord`]s.
//!
//! Read at the winit layer rather than out of egui's event list, for one
//! reason: delightfile's keymap owns the keyboard completely (PLAN §4), and
//! egui's own text handling would eat `Space`, `Tab` and arrows on its way past.
//! When the vi line editor arrives (§4.2) it will be fed from the same place, so
//! there is one path from "a key went down" to "something happened".
//!
//! The translation itself is one rule, and it is df-core's rule, not this
//! file's: **a key is stored unshifted, and Shift lives in the modifiers** (see
//! [`df_core::keymap::Key`]). winit's `key_without_modifiers()` reports exactly
//! that unshifted key, so `<` arrives as `Char(',')` with Shift from the
//! modifier state and can never become a second, unrelated binding depending on
//! which spelling the compositor felt like.

use df_core::keymap::{Chord, Key, Mods};
use winit::event::KeyEvent;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

/// The chord this keystroke is, if it is one delightfile can bind.
///
/// Modifier presses themselves are `None`: holding Ctrl is not a keystroke, and
/// treating it as one would abandon every pending chord the moment a hand
/// reached for the next key.
pub fn chord(event: &KeyEvent, mods: ModifiersState) -> Option<Chord> {
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    // `key_without_modifiers` is the whole normalization: it undoes Shift, Caps
    // Lock and Ctrl, so the layout question is answered once, by the compositor,
    // rather than by a shift table here. `logical_key` is the fallback for the
    // platforms that do not implement it.
    chord_from(&event.key_without_modifiers(), mods).or_else(|| chord_from(&event.logical_key, mods))
}

/// What this keystroke *types*, if it types anything.
///
/// The same press is both a chord and — when something is open to type into —
/// a character (see `app::Press`). winit reports the text a key produces
/// including the control characters: `Esc` is `\u{1b}`, `Enter` is `\r`, `Tab`
/// is `\t`. Those are keystrokes, not text, and a filter query with an escape
/// character in it is a query that quietly stops matching, so anything with a
/// control character in it is not text at all.
pub fn text(event: &KeyEvent) -> Option<String> {
    let text = event.text.as_deref()?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    Some(text.to_string())
}

/// The pure half, so the mapping can be tested without a window.
pub fn chord_from(key: &WinitKey, mods: ModifiersState) -> Option<Chord> {
    let (key, implied_shift) = translate(key)?;
    Some(Chord {
        mods: Mods {
            ctrl: mods.control_key(),
            alt: mods.alt_key(),
            // `implied_shift` covers the fallback path, where a compositor
            // handed over the *shifted* glyph: `<` implies Shift whatever the
            // modifier state says, because it cannot be typed without one.
            shift: mods.shift_key() || implied_shift,
            super_key: mods.super_key(),
        },
        key,
    })
}

/// One winit key → one df-core key, plus whether the glyph itself implies
/// Shift.
fn translate(key: &WinitKey) -> Option<(Key, bool)> {
    match key {
        WinitKey::Character(text) => {
            let mut chars = text.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                // A dead key or a composed sequence. Not bindable, and silently
                // ignoring it is better than binding half of it.
                return None;
            };
            Key::from_char(c)
        }
        WinitKey::Named(named) => named_key(*named).map(|k| (k, false)),
        // `Unidentified` and `Dead` are keys the compositor could not name.
        _ => None,
    }
}

fn named_key(named: NamedKey) -> Option<Key> {
    Some(match named {
        NamedKey::Escape => Key::Escape,
        NamedKey::Enter => Key::Enter,
        NamedKey::Tab => Key::Tab,
        NamedKey::Backspace => Key::Backspace,
        NamedKey::Delete => Key::Delete,
        NamedKey::Insert => Key::Insert,
        NamedKey::Space => Key::Space,
        NamedKey::ArrowUp => Key::ArrowUp,
        NamedKey::ArrowDown => Key::ArrowDown,
        NamedKey::ArrowLeft => Key::ArrowLeft,
        NamedKey::ArrowRight => Key::ArrowRight,
        NamedKey::Home => Key::Home,
        NamedKey::End => Key::End,
        NamedKey::PageUp => Key::PageUp,
        NamedKey::PageDown => Key::PageDown,
        NamedKey::F1 => Key::F(1),
        NamedKey::F2 => Key::F(2),
        NamedKey::F3 => Key::F(3),
        NamedKey::F4 => Key::F(4),
        NamedKey::F5 => Key::F(5),
        NamedKey::F6 => Key::F(6),
        NamedKey::F7 => Key::F(7),
        NamedKey::F8 => Key::F(8),
        NamedKey::F9 => Key::F(9),
        NamedKey::F10 => Key::F(10),
        NamedKey::F11 => Key::F(11),
        NamedKey::F12 => Key::F(12),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn character(text: &str) -> WinitKey {
        WinitKey::Character(text.into())
    }

    #[test]
    fn a_plain_letter_is_a_plain_chord() {
        assert_eq!(
            chord_from(&character("g"), ModifiersState::empty()),
            Some(Chord::plain(Key::Char('g')))
        );
    }

    /// The rule the module exists for: the unshifted key plus a Shift, which is
    /// what `G` and `<` both are.
    #[test]
    fn shift_is_a_modifier_not_a_different_key() {
        let g = chord_from(&character("g"), ModifiersState::SHIFT).expect("G");
        assert_eq!(g, Chord::shift(Key::Char('g')));
        assert_eq!(g.label(), "G");
        let less = chord_from(&character(","), ModifiersState::SHIFT).expect("<");
        assert_eq!(less.key, Key::Char(','));
        assert_eq!(less.label(), "<");
    }

    /// The fallback path: a compositor that reported the *shifted* glyph must
    /// still land on the same chord as the one that reported the key.
    #[test]
    fn a_shifted_glyph_implies_its_own_shift() {
        let from_glyph = chord_from(&character("<"), ModifiersState::empty()).expect("<");
        let from_key = chord_from(&character(","), ModifiersState::SHIFT).expect("<");
        assert_eq!(from_glyph, from_key);
    }

    #[test]
    fn modifiers_carry_through() {
        let ctrl_u = chord_from(&character("u"), ModifiersState::CONTROL).expect("Ctrl+u");
        assert_eq!(ctrl_u, Chord::ctrl(Key::Char('u')));
        let alt_left = chord_from(
            &WinitKey::Named(NamedKey::ArrowLeft),
            ModifiersState::ALT,
        )
        .expect("Alt+←");
        assert_eq!(alt_left, Chord::alt(Key::ArrowLeft));
        assert_eq!(alt_left.label(), "Alt+←");
    }

    #[test]
    fn the_named_keys_the_default_map_uses_all_translate() {
        for (named, expected) in [
            (NamedKey::Escape, Key::Escape),
            (NamedKey::Enter, Key::Enter),
            (NamedKey::Tab, Key::Tab),
            (NamedKey::Space, Key::Space),
            (NamedKey::ArrowUp, Key::ArrowUp),
            (NamedKey::ArrowDown, Key::ArrowDown),
            (NamedKey::PageUp, Key::PageUp),
            (NamedKey::PageDown, Key::PageDown),
            (NamedKey::F1, Key::F(1)),
        ] {
            assert_eq!(
                chord_from(&WinitKey::Named(named), ModifiersState::empty()),
                Some(Chord::plain(expected)),
                "{named:?}"
            );
        }
    }

    /// Holding a modifier is not a keystroke — if it were, reaching for `Ctrl`
    /// would abandon a chord that was half typed.
    #[test]
    fn modifier_presses_are_not_chords() {
        assert_eq!(
            chord_from(&WinitKey::Named(NamedKey::Control), ModifiersState::CONTROL),
            None
        );
        assert_eq!(
            chord_from(&WinitKey::Named(NamedKey::Shift), ModifiersState::SHIFT),
            None
        );
        // …and neither is a composed multi-character input.
        assert_eq!(chord_from(&character("ab"), ModifiersState::empty()), None);
    }
}
