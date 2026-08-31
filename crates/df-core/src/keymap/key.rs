//! The key model: what a keystroke *is*, before anything decides what it means.
//!
//! df-core may not depend on egui (PLAN §1), so delightviewer's `egui::Key` is
//! replaced by a small enum of our own. That turns out to be the better model
//! anyway, because of the one rule this file exists to enforce:
//!
//! **A key is stored unshifted, and Shift lives in the modifiers.** `<` is
//! Shift + `,` on every layout delightfile runs on, so it is *stored* as
//! `Chord { shift, Char(',') }` — but it is *written* and *displayed* as `<`,
//! because that is the glyph on the key and the thing the hand does. Without
//! this normalization the sort chord `,` and the skip-back `<` would be two
//! unrelated bindings that fight over the same physical key depending on which
//! spelling the compositor happened to report, and `?` (Shift+`/`) would go
//! unbound on exactly one Wayland compositor — the bug delightviewer's keymap
//! carries three separate bindings to work around.
//!
//! The same rule covers letters: `G` is `Chord { shift, Char('g') }`, printed
//! `G`. A help sheet that says "Shift+G" is the same instruction written twice.

use std::fmt;

/// A physical key, named by its unshifted glyph where it has one.
///
/// `Char` holds the **unshifted** character: lowercase for letters, the base
/// glyph for punctuation (`,` never `<`, `[` never `{`). Anything with no
/// glyph gets a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// A printable key, stored unshifted. See the module header.
    Char(char),
    /// `F1`–`F24`.
    F(u8),
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Space,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
}

impl Key {
    /// The key a character is typed on, and whether typing it needs Shift.
    ///
    /// `'G'` → `(Char('g'), true)`; `'<'` → `(Char(','), true)`; `'g'` →
    /// `(Char('g'), false)`. This is the normalization the module header is
    /// about, in one function, so there is exactly one place that knows the
    /// US-layout shift table.
    pub fn from_char(c: char) -> Option<(Key, bool)> {
        if c == ' ' {
            return Some((Key::Space, false));
        }
        if c.is_ascii_uppercase() {
            return Some((Key::Char(c.to_ascii_lowercase()), true));
        }
        if let Some(base) = unshift(c) {
            return Some((Key::Char(base), true));
        }
        if c.is_ascii_graphic() {
            return Some((Key::Char(c), false));
        }
        None
    }

    /// The glyph this key produces with Shift held, if it has one.
    pub fn shifted_glyph(self) -> Option<char> {
        match self {
            Key::Char(c) if c.is_ascii_lowercase() => Some(c.to_ascii_uppercase()),
            Key::Char(c) => shift(c),
            _ => None,
        }
    }

    /// Name → key, for `keymap.toml`. Named keys are matched
    /// case-insensitively (`Esc`, `esc`, `ESC`); a single character goes
    /// through [`Key::from_char`], which is case-*sensitive* because that is
    /// how `g` and `G` stay two bindings.
    pub fn from_name(name: &str) -> Option<(Key, bool)> {
        // `<Esc>` / `<C-a>`-style angle brackets are how yazi and vim spell
        // these, and they are what muscle memory types into a config file.
        // …but `<` and `>` are themselves keys (the ±10 s skips), so only a
        // wrapper with something inside it is a wrapper.
        let name = match name.strip_prefix('<').and_then(|n| n.strip_suffix('>')) {
            Some(inner) if !inner.is_empty() => inner,
            _ => name,
        };
        let mut chars = name.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Key::from_char(c);
        }
        let lowered = name.to_ascii_lowercase();
        if let Some(n) = lowered.strip_prefix('f') {
            if let Ok(n) = n.parse::<u8>() {
                if (1..=24).contains(&n) {
                    return Some((Key::F(n), false));
                }
            }
        }
        let key = match lowered.as_str() {
            "space" => Key::Space,
            "tab" => Key::Tab,
            "enter" | "return" | "cr" => Key::Enter,
            "esc" | "escape" => Key::Escape,
            "backspace" | "bs" => Key::Backspace,
            "delete" | "del" => Key::Delete,
            "insert" | "ins" => Key::Insert,
            "up" | "arrowup" => Key::ArrowUp,
            "down" | "arrowdown" => Key::ArrowDown,
            "left" | "arrowleft" => Key::ArrowLeft,
            "right" | "arrowright" => Key::ArrowRight,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" | "pgup" => Key::PageUp,
            "pagedown" | "pgdn" => Key::PageDown,
            _ => return None,
        };
        Some((key, false))
    }

    /// What this key is *called* — the unshifted form, for the help sheet.
    pub fn label(self) -> String {
        match self {
            Key::Char(c) => c.to_string(),
            Key::F(n) => format!("F{n}"),
            Key::Escape => "Esc".to_string(),
            Key::Enter => "Enter".to_string(),
            Key::Tab => "Tab".to_string(),
            Key::Backspace => "Backspace".to_string(),
            Key::Delete => "Del".to_string(),
            Key::Insert => "Ins".to_string(),
            Key::Space => "Space".to_string(),
            // Arrows are drawn, not spelled: the help sheet and the which-key
            // card are dense grids, and "ArrowLeft" is four times the width of
            // the thing it describes.
            Key::ArrowUp => "↑".to_string(),
            Key::ArrowDown => "↓".to_string(),
            Key::ArrowLeft => "←".to_string(),
            Key::ArrowRight => "→".to_string(),
            Key::Home => "Home".to_string(),
            Key::End => "End".to_string(),
            Key::PageUp => "PgUp".to_string(),
            Key::PageDown => "PgDn".to_string(),
        }
    }
}

/// Shifted glyph → the key it lives on. The US layout, which is the one every
/// machine this runs on is set to.
fn unshift(c: char) -> Option<char> {
    Some(match c {
        '~' => '`',
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        _ => return None,
    })
}

/// The inverse of [`unshift`], for printing a chord back out.
fn shift(c: char) -> Option<char> {
    Some(match c {
        '`' => '~',
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        _ => return None,
    })
}

/// The modifiers held with a key.
///
/// `super_key` is the Super/Windows/Meta key. It is here although no default
/// binding uses one: the compositor owns Super on this machine, but the field
/// is what lets a user *say* Super in `keymap.toml` and get a warning about the
/// compositor rather than a chord that silently never fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: false,
        super_key: false,
    };
    pub const SHIFT: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: true,
        super_key: false,
    };
    pub const CTRL: Mods = Mods {
        ctrl: true,
        alt: false,
        shift: false,
        super_key: false,
    };
    pub const ALT: Mods = Mods {
        ctrl: false,
        alt: true,
        shift: false,
        super_key: false,
    };

    pub fn is_none(self) -> bool {
        self == Mods::NONE
    }

    /// How hard this is to press, for choosing which binding the help sheet
    /// advertises. **Shift is not the same cost as Ctrl**: a capital is one
    /// hand and the thing you do to type your own name, while Ctrl and Alt are
    /// a reach. Counting them alike teaches `Ctrl+r` for a command that is also
    /// on `R`. (Ported from delightviewer's `binding_label`.)
    pub fn weight(self) -> u8 {
        self.shift as u8 + 2 * self.ctrl as u8 + 2 * self.alt as u8 + 2 * self.super_key as u8
    }
}

/// One keystroke: a key and the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

impl Chord {
    pub const fn new(mods: Mods, key: Key) -> Chord {
        Chord { mods, key }
    }

    pub const fn plain(key: Key) -> Chord {
        Chord {
            mods: Mods::NONE,
            key,
        }
    }

    pub const fn shift(key: Key) -> Chord {
        Chord {
            mods: Mods::SHIFT,
            key,
        }
    }

    pub const fn ctrl(key: Key) -> Chord {
        Chord {
            mods: Mods::CTRL,
            key,
        }
    }

    pub const fn alt(key: Key) -> Chord {
        Chord {
            mods: Mods::ALT,
            key,
        }
    }

    /// The chord that types `c` — the normalization from the module header.
    /// `Chord::from_char('<')` is Shift+`,`, and prints itself as `<`.
    pub fn from_char(c: char) -> Option<Chord> {
        let (key, shifted) = Key::from_char(c)?;
        Some(Chord {
            mods: Mods {
                shift: shifted,
                ..Mods::NONE
            },
            key,
        })
    }

    /// Human-readable, and **the case is the key you press**: a letter prints
    /// lowercase unless the binding carries Shift, in which case the capital
    /// *is* the Shift and the prefix comes off. Shifted punctuation prints its
    /// shifted glyph for the same reason — `<`, not `Shift+,`.
    pub fn label(&self) -> String {
        let mut out = String::new();
        if self.mods.ctrl {
            out.push_str("Ctrl+");
        }
        if self.mods.alt {
            out.push_str("Alt+");
        }
        if self.mods.super_key {
            out.push_str("Super+");
        }
        if self.mods.shift {
            match self.key.shifted_glyph() {
                Some(glyph) => out.push(glyph),
                None => {
                    out.push_str("Shift+");
                    out.push_str(&self.key.label());
                }
            }
        } else {
            out.push_str(&self.key.label());
        }
        out
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// Everything that can go wrong turning text into a binding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeymapError {
    #[error("`{0}` is not a key chord")]
    BadChord(String),
    #[error("`{0}` is reserved for media transport (PLAN §4.3) and may only be bound globally")]
    ReservedKey(String),
    #[error("`{0}` is already bound in this context")]
    Conflict(String),
    #[error("unknown command `{0}`")]
    UnknownCommand(String),
    #[error("unknown context `{0}`")]
    UnknownContext(String),
}

/// Render a sequence the way the help sheet and which-key card do: `g g`.
pub fn label_sequence(seq: &[Chord]) -> String {
    seq.iter().map(Chord::label).collect::<Vec<_>>().join(" ")
}

/// Parse a whitespace-separated chord sequence: `"g g"`, `"ctrl+p"`, `", m"`.
///
/// A literal space cannot be a separator *and* a key, so the Space key is
/// written by name: `"g space"`.
pub fn parse_sequence(text: &str) -> Result<Vec<Chord>, KeymapError> {
    let seq = text
        .split_whitespace()
        .map(parse_chord)
        .collect::<Result<Vec<Chord>, KeymapError>>()?;
    if seq.is_empty() {
        return Err(KeymapError::BadChord(text.to_string()));
    }
    Ok(seq)
}

/// Parse one chord: `ctrl+shift+p`, `alt+left`, `G`, `<`, `f1`.
pub fn parse_chord(text: &str) -> Result<Chord, KeymapError> {
    let bad = || KeymapError::BadChord(text.to_string());
    let mut mods = Mods::NONE;
    let mut key: Option<Key> = None;

    // `+` is both the modifier separator and a key. Two empty trailing fields
    // is the unambiguous signature of the key itself (`"+"` → `["", ""]`,
    // `"ctrl++"` → `["ctrl", "", ""]`); one is a trailing separator, i.e. a
    // typo. (Ported from delightviewer.)
    let mut parts: Vec<&str> = text.split('+').collect();
    if parts.len() >= 2 && parts[parts.len() - 1].is_empty() && parts[parts.len() - 2].is_empty() {
        parts.truncate(parts.len() - 2);
        parts.push("+");
    }

    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            // Only the spelled-out names are modifiers: `c` has to stay the
            // letter, or the copy chords (`c c`, `c t`) become unwritable.
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "meta" => mods.alt = true,
            "shift" => mods.shift = true,
            "super" | "win" | "cmd" | "logo" => mods.super_key = true,
            _ => {
                if key.is_some() {
                    return Err(bad());
                }
                let (k, implied_shift) = Key::from_name(part).ok_or_else(bad)?;
                if implied_shift {
                    mods.shift = true;
                }
                key = Some(k);
            }
        }
    }
    Ok(Chord {
        mods,
        key: key.ok_or_else(bad)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule this module exists for: a shifted glyph binds as itself and is
    /// stored as Shift + the unshifted key, so the two spellings of the same
    /// keystroke can never become two bindings.
    #[test]
    fn shifted_glyphs_normalize_to_shift_plus_base_key() {
        for (glyph, base) in [
            ('<', ','),
            ('>', '.'),
            ('?', '/'),
            (':', ';'),
            ('{', '['),
            ('}', ']'),
            ('_', '-'),
            ('+', '='),
            ('~', '`'),
            ('|', '\\'),
            ('"', '\''),
        ] {
            let chord = parse_chord(&glyph.to_string()).expect("parses");
            assert_eq!(chord.key, Key::Char(base), "{glyph}");
            assert!(chord.mods.shift, "{glyph} must carry Shift");
            // …and it prints back as the glyph on the key, never "Shift+,".
            assert_eq!(chord.label(), glyph.to_string());
        }
    }

    #[test]
    fn a_capital_is_shift_and_prints_as_a_capital() {
        let g = parse_chord("G").expect("parses");
        assert_eq!(g, Chord::shift(Key::Char('g')));
        assert_eq!(g.label(), "G");
        assert_eq!(parse_chord("g").expect("parses").label(), "g");
        // `shift+g` is the long way of writing the same keystroke.
        assert_eq!(parse_chord("shift+g").expect("parses"), g);
    }

    #[test]
    fn modifier_spellings() {
        assert_eq!(
            parse_chord("ctrl+p").expect("parses"),
            Chord::ctrl(Key::Char('p'))
        );
        assert_eq!(
            parse_chord("Alt+Left").expect("parses"),
            Chord::alt(Key::ArrowLeft)
        );
        assert_eq!(
            parse_chord("ctrl+shift+z").expect("parses").label(),
            // The capital *is* the Shift, even with a Ctrl in front of it.
            "Ctrl+Z"
        );
        assert_eq!(parse_chord("f1").expect("parses").key, Key::F(1));
        assert_eq!(parse_chord("<Esc>").expect("parses").key, Key::Escape);
        assert_eq!(parse_chord("+").expect("parses").label(), "+");
        assert_eq!(parse_chord("ctrl+-").expect("parses").label(), "Ctrl+-");
    }

    #[test]
    fn sequences_split_on_whitespace() {
        let seq = parse_sequence("g g").expect("parses");
        assert_eq!(seq.len(), 2);
        assert_eq!(label_sequence(&seq), "g g");
        assert_eq!(
            label_sequence(&parse_sequence(", m").expect("parses")),
            ", m"
        );
        assert_eq!(
            parse_sequence("g space").expect("parses")[1].key,
            Key::Space
        );
    }

    #[test]
    fn nonsense_is_an_error_not_a_guess() {
        for text in ["", "  ", "ctrl+", "notakey", "ctrl+a+b"] {
            assert!(parse_sequence(text).is_err(), "`{text}` should not parse");
        }
    }

    /// Shift costs less than Ctrl — the weight that decides which binding the
    /// help sheet teaches.
    #[test]
    fn shift_is_cheaper_than_ctrl() {
        assert!(Mods::SHIFT.weight() < Mods::CTRL.weight());
        assert_eq!(Mods::NONE.weight(), 0);
    }
}
