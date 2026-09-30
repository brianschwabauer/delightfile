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

/// A key going down, as the window reads it: the parts of winit's
/// [`KeyEvent`] a chord and a text are made from.
///
/// Its own type rather than the event, because a `KeyEvent` cannot be made
/// outside winit (its platform half is private), and a test has to be able
/// to press the key a platform actually delivers — on Windows a letter
/// carries its text even with Ctrl held, where Wayland hands over a control
/// character — and follow it all the way to the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stroke {
    /// The key with no modifier applied (`key_without_modifiers`): what the
    /// chord is read from first.
    pub unmodified: WinitKey,
    /// The key as the layout and the modifiers made it (`logical_key`): the
    /// fallback where a platform has no unmodified key to give.
    pub logical: WinitKey,
    /// What the key types, as winit reports it, control characters and all.
    pub text: Option<String>,
    /// winit's own answer to "is this key repeating".
    pub repeat: bool,
}

impl Stroke {
    /// The parts of a real event.
    pub fn of(event: &KeyEvent) -> Stroke {
        use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
        Stroke {
            unmodified: event.key_without_modifiers(),
            logical: event.logical_key.clone(),
            text: event.text.as_ref().map(|text| text.to_string()),
            repeat: event.repeat,
        }
    }
}

/// The chord this keystroke is, if it is one delightfile can bind.
///
/// Modifier presses themselves are `None`: holding Ctrl is not a keystroke, and
/// treating it as one would abandon every pending chord the moment a hand
/// reached for the next key.
pub fn chord(stroke: &Stroke, mods: ModifiersState) -> Option<Chord> {
    // `key_without_modifiers` is the whole normalization: it undoes Shift, Caps
    // Lock and Ctrl, so the layout question is answered once, by the compositor,
    // rather than by a shift table here. `logical_key` is the fallback for the
    // platforms that do not implement it.
    chord_from(&stroke.unmodified, mods).or_else(|| chord_from(&stroke.logical, mods))
}

/// What this keystroke *types*, if it types anything.
///
/// The same press is both a chord and — when something is open to type into —
/// a character (see `app::Press`). winit reports the text a key produces
/// including the control characters: `Esc` is `\u{1b}`, `Enter` is `\r`, `Tab`
/// is `\t`. Those are keystrokes, not text, and a filter query with an escape
/// character in it is a query that quietly stops matching, so anything with a
/// control character in it is not text at all.
pub fn text(stroke: &Stroke) -> Option<String> {
    let text = stroke.text.as_deref()?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    Some(text.to_string())
}

/// A chord as the keymap spells it (`ctrl+s`, `alt+enter`) written the way
/// this platform writes chords — `Ctrl+s` on Linux, `⌘s` on a Mac
/// ([`df_core::platform::keys::LABELS`], M2.21) — for the few places that
/// name a key in running text or a card's `&'static str` hint rather than
/// asking the keymap. Each chord is written once per process and kept; text
/// that is not a chord comes back as it is.
pub fn written(chord: &'static str) -> &'static str {
    use std::sync::Mutex;
    static WRITTEN: Mutex<Vec<(&str, &str)>> = Mutex::new(Vec::new());
    let mut written = match WRITTEN.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some((_, label)) = written.iter().find(|(text, _)| *text == chord) {
        return label;
    }
    let Ok(parsed) = df_core::keymap::parse_chord(chord) else {
        return chord;
    };
    let label: &'static str = Box::leak(parsed.label().into_boxed_str());
    written.push((chord, label));
    label
}

/// The pure half, so the mapping can be tested without a window.
///
/// Which physical key is which modifier is the platform's
/// ([`crate::platform::keys::mods`]): on macOS Command is `ctrl`.
pub fn chord_from(key: &WinitKey, mods: ModifiersState) -> Option<Chord> {
    let (key, implied_shift) = translate(key)?;
    let held = crate::platform::keys::mods(mods);
    Some(Chord {
        mods: Mods {
            // `implied_shift` covers the fallback path, where a compositor
            // handed over the *shifted* glyph: `<` implies Shift whatever the
            // modifier state says, because it cannot be typed without one.
            shift: held.shift || implied_shift,
            ..held
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
        let alt_left =
            chord_from(&WinitKey::Named(NamedKey::ArrowLeft), ModifiersState::ALT).expect("Alt+←");
        assert_eq!(alt_left, Chord::alt(Key::ArrowLeft));
        // Written the platform's way (M2.21): the key caps' `⌥` on a Mac.
        let written = if cfg!(target_os = "macos") {
            "⌥←"
        } else {
            "Alt+←"
        };
        assert_eq!(alt_left.label(), written);
    }

    /// A chord named in running text is written as the keymap writes it,
    /// once, and anything that is not a chord is left alone.
    #[test]
    fn a_named_chord_is_written_the_platforms_way() {
        let (stop, other) = if cfg!(target_os = "macos") {
            ("⌘s", "⌘N")
        } else {
            ("Ctrl+s", "Ctrl+N")
        };
        assert_eq!(written("ctrl+s"), stop);
        assert!(std::ptr::eq(written("ctrl+s"), written("ctrl+s")));
        assert_eq!(written("ctrl+N"), other);
        assert_eq!(written("alt+enter"), Chord::alt(Key::Enter).label());
        assert_eq!(written("not a chord"), "not a chord");
    }

    /// Command is Ctrl on macOS (`plans/other-platforms/02-macos.md` M2.20),
    /// so a keymap's `ctrl+…` answers to it; everywhere else Super is Super.
    #[test]
    fn super_is_ctrl_on_macos_and_itself_elsewhere() {
        let chord = chord_from(&character("c"), ModifiersState::SUPER).expect("Super+c");
        if cfg!(target_os = "macos") {
            assert_eq!(chord, Chord::ctrl(Key::Char('c')));
        } else {
            assert!(chord.mods.super_key && !chord.mods.ctrl, "{chord:?}");
        }
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
