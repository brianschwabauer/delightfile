//! The line editor behind every prompt (PLAN §4.2).
//!
//! Rename, filter, create, `cd`, search and shell all put a single line of text
//! in front of the user, and they all get the same editor. It is a **plain**
//! one: keys type, `Esc` closes, and there is no mode to be in. yazi's modal
//! `i I a A v r` ladder used to live here and is gone — the one thing it bought
//! was `cw` in a rename, and the price was a prompt where the letter you typed
//! after a stray `Esc` silently ran a command instead of appearing.
//!
//! What survives is everything that is *editing* rather than *moding*: word
//! motions, the readline kill family, a shift-extended selection and undo. Those
//! are the keys a person actually reaches for while renaming
//! `2024-holiday-DSC_0198.jpeg`, and none of them need a mode to be reachable.
//!
//! This module is the whole editor as a **pure state machine**: chords in,
//! [`InputEvent`] out, and three accessors ([`InputBuffer::text`],
//! [`InputBuffer::cursor`], [`InputBuffer::selection`]) that the UI paints from.
//! It knows nothing about popups, prompts or where the text is going, which is
//! why it can be exercised exhaustively by `cargo test` with no display attached
//! (PLAN §1).
//!
//! # The bindings
//!
//! Readline's, because that is the vocabulary every text field on a unix desktop
//! already answers to and the one a shell user has in their fingers:
//!
//! * `Ctrl+a` / `Home` and `Ctrl+e` / `End` — the ends of the line.
//! * `Ctrl+b` / `Ctrl+f` and the arrows — one character.
//! * `Alt+b` / `Alt+f` and `Ctrl+`arrow — one word.
//! * `Ctrl+w` kills the word behind, `Alt+d` the word ahead, `Ctrl+u` back to
//!   the start of the line, `Ctrl+k` on to its end.
//! * `Ctrl+h` / `Backspace` and `Ctrl+d` / `Delete` — one character.
//! * `Ctrl+z` undoes, `Ctrl+y` and `Ctrl+Shift+z` redo.
//! * `Shift` on any motion extends the selection instead of dropping it; typing
//!   or deleting over a selection replaces it, as in every other text field.
//! * `Esc` and `Ctrl+c` cancel. Always, first press.
//!
//! # Unicode
//!
//! Positions are **char (Unicode scalar) indices**, not bytes and not extended
//! grapheme clusters. Bytes are wrong — a left-arrow over `é` would split it and
//! panic. Graphemes would be righter still (a flag emoji is seven scalars and
//! one thing you point at), but they need a segmentation table that is a
//! dependency this crate will not take (PLAN §1), and the payload here is
//! *filenames*, where a combining sequence is rare and a family-emoji ZWJ chain
//! is rarer. Scalar boundaries never panic and never corrupt the string; they
//! can only cost an extra press on text almost nobody types.
//!
//! Word motions are class-based over `char::is_alphanumeric`, so `naïve_café`
//! is one word and `日本語` is one word — the multibyte case is handled by
//! asking the character what it is, never by counting bytes.

use crate::keymap::{Chord, Key, Mods};

#[cfg(test)]
mod tests;

/// How many undo steps a prompt keeps.
///
/// A prompt lives for seconds and holds one line; 256 edits is far past any
/// session that ends in a filename, and the cap exists only so a key-repeat
/// storm cannot grow the stack without bound. Each entry is a copy of the line,
/// so the worst case is a few tens of kilobytes — cheaper than a smarter
/// structure would be to read.
pub const UNDO_LIMIT: usize = 256;

/// What a keystroke did to the prompt, from the caller's point of view.
///
/// There is no `Ignored`: an open prompt swallows every key it is handed, so
/// that a stray `q` while renaming types a `q` instead of quitting the
/// application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// The buffer handled it. Repaint from the accessors.
    Consumed,
    /// `Enter`: the user is done. Carries the final text.
    Submit(String),
    /// `Esc` or `Ctrl+c`. Throw the prompt away.
    Cancel,
}

/// A saved line, for undo and redo.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
}

/// What class a character belongs to, for word motions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Space,
    Punct,
    Word,
}

fn kind(c: char) -> CharKind {
    if c.is_whitespace() {
        CharKind::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharKind::Word
    } else {
        CharKind::Punct
    }
}

/// One line of text being edited.
///
/// Construct with [`InputBuffer::new`] (or a rename preset), feed it
/// [`Chord`]s, paint from the accessors.
#[derive(Debug, Clone)]
pub struct InputBuffer {
    text: String,
    /// Char index, from 0 to the line's length inclusive — the caret sits
    /// *between* characters, always.
    cursor: usize,
    /// The other end of a shift-extended selection, when there is one. `None`
    /// is "nothing selected"; an anchor that has landed back on the cursor is
    /// reported as no selection rather than as an empty range.
    anchor: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Set while a run of typing is in progress, so that a word of typing is
    /// one undo step rather than one per keystroke.
    tagged: bool,
}

impl InputBuffer {
    /// A prompt over `initial`, with the caret at char index `cursor_pos`
    /// (clamped) and nothing selected.
    pub fn new(initial: impl Into<String>, cursor_pos: usize) -> InputBuffer {
        let text = initial.into();
        let len = text.chars().count();
        InputBuffer {
            text,
            cursor: cursor_pos.min(len),
            anchor: None,
            undo: Vec::new(),
            redo: Vec::new(),
            tagged: false,
        }
    }

    /// `r` on a file: the whole name, caret **before the extension** — yazi's
    /// `rename --cursor=before_ext` (PLAN §4.1).
    ///
    /// A leading dot is part of the name, not an extension: `.bashrc` renames
    /// with the caret at the end, because "the extension of `.bashrc`" is a
    /// question only a parser asks.
    pub fn for_rename_stem(name: &str) -> InputBuffer {
        let chars: Vec<char> = name.chars().collect();
        let dot = chars
            .iter()
            .rposition(|&c| c == '.')
            .filter(|&i| i > 0)
            .unwrap_or(chars.len());
        InputBuffer::new(name, dot)
    }

    /// `R` on a file: the stem emptied, the extension kept, caret at the start
    /// — yazi's `rename --empty=stem --cursor=start` (PLAN §4.1).
    ///
    /// `photo.jpg` opens as `.jpg` with the caret at 0, so the new name is
    /// typed straight in front of the extension you wanted to keep.
    pub fn for_rename_empty(name: &str) -> InputBuffer {
        let chars: Vec<char> = name.chars().collect();
        let ext: String = match chars.iter().rposition(|&c| c == '.').filter(|&i| i > 0) {
            Some(i) => chars[i..].iter().collect(),
            None => String::new(),
        };
        InputBuffer::new(ext, 0)
    }

    // ── Accessors: everything the UI needs to paint ─────────────────────────

    /// The line as it stands.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Caret position as a **char index**. See the module header on Unicode.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Caret position as a byte offset into [`InputBuffer::text`], for a
    /// renderer that measures with `&text[..cursor_byte()]`.
    pub fn cursor_byte(&self) -> usize {
        self.byte_of(self.cursor)
    }

    /// The live selection as a half-open **char** range, or `None` when
    /// nothing is selected.
    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            return None;
        }
        let (lo, hi) = if anchor < self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        Some(lo..hi)
    }

    /// The same range in bytes, for slicing [`InputBuffer::text`] directly.
    pub fn selection_bytes(&self) -> Option<std::ops::Range<usize>> {
        let r = self.selection()?;
        Some(self.byte_of(r.start)..self.byte_of(r.end))
    }

    // ── The dispatcher ──────────────────────────────────────────────────────

    /// Feed one keystroke.
    ///
    /// Takes a [`Chord`] rather than a bare [`Key`] because half of the map is
    /// modified — `Alt+b` and `Ctrl+b` are different commands — and a `Key`
    /// alone cannot tell them apart.
    pub fn feed(&mut self, chord: Chord) -> InputEvent {
        // Text first: an unmodified printable key is always *text*, which is
        // the whole point of there being no Normal mode. Modified chords fall
        // through to the map, so `Ctrl+u` still kills to the start of the line
        // and the arrows still move.
        if !chord.mods.ctrl && !chord.mods.alt && !chord.mods.super_key {
            if let Some(c) = printable(chord) {
                self.type_char(c);
                return InputEvent::Consumed;
            }
        }
        self.command(chord)
    }

    /// Feed already-composed text at the caret: an IME commit, the result of a
    /// dead-key sequence on a non-US layout, or a paste.
    ///
    /// [`InputBuffer::feed`] cannot serve these. It takes a [`Chord`], and a
    /// chord is one key: `´` then `e` on a French layout is two keystrokes and
    /// one character, `私` is a whole conversion the compositor hands over in
    /// one string, and a paste is a hundred characters with no keystroke at
    /// all. Every one of them arrives on the window system's *text* channel
    /// (winit's `Ime::Commit`, the clipboard), not its key channel, so the
    /// buffer needs a door on that side too.
    ///
    /// A live selection is replaced, exactly as typing the characters one at a
    /// time would replace it, and the whole commit lands inside the typing run
    /// it arrives in, so `Ctrl+z` takes back the word rather than splitting it
    /// around the composition.
    ///
    /// The text is taken verbatim, newlines and all — this buffer is a string,
    /// not a filename validator, and a caller that does not want the trailing
    /// `\n` off a clipboard should trim it before it gets here.
    pub fn insert_text(&mut self, text: &str) -> InputEvent {
        if text.is_empty() {
            return InputEvent::Consumed;
        }
        self.tag_once();
        let at = self.take_selection();
        self.splice(at, at, text);
        self.cursor = at + text.chars().count();
        InputEvent::Consumed
    }

    /// The keymap, transcribed.
    fn command(&mut self, chord: Chord) -> InputEvent {
        let m = chord.mods;
        let plain = m.is_none();
        let shift = m == Mods::SHIFT;
        let ctrl = m == Mods::CTRL;
        let ctrl_shift = m.ctrl && m.shift && !m.alt && !m.super_key;
        let alt = m == Mods::ALT;
        // A motion extends the selection when Shift is down and drops it
        // otherwise — the rule every other text field on the desktop shares.
        let extend = m.shift;

        match chord.key {
            // ── Close ───────────────────────────────────────────────────────
            Key::Char('c') if ctrl => return InputEvent::Cancel,
            Key::Enter if plain => return InputEvent::Submit(self.text.clone()),
            // `Esc` means "close this" here and in every other dialog in the
            // program, on the first press. `<C-[>` is the same keystroke on a
            // terminal and stays the same command here.
            Key::Escape if plain => return InputEvent::Cancel,
            Key::Char('[') if ctrl => return InputEvent::Cancel,

            // ── Word-wise movement ──────────────────────────────────────────
            // Before the character arrows, because `Ctrl+Left` is a word and
            // the bare arrow's arm would otherwise have to exclude it.
            Key::ArrowLeft if ctrl || ctrl_shift => self.move_to(self.backward(), extend),
            Key::ArrowRight if ctrl || ctrl_shift => self.move_to(self.forward(), extend),
            Key::Char('b') if alt => self.move_to(self.backward(), false),
            Key::Char('f') if alt => self.move_to(self.forward(), false),

            // ── Character-wise movement ─────────────────────────────────────
            Key::ArrowLeft if plain || shift => self.move_to(self.left(extend), extend),
            Key::ArrowRight if plain || shift => self.move_to(self.right(extend), extend),
            Key::Char('b') if ctrl => self.move_to(self.left(false), false),
            Key::Char('f') if ctrl => self.move_to(self.right(false), false),

            // ── Line-wise movement ──────────────────────────────────────────
            Key::Home if plain || shift => self.move_to(0, extend),
            Key::End if plain || shift => self.move_to(self.len(), extend),
            Key::Char('a') if ctrl => self.move_to(0, false),
            Key::Char('e') if ctrl => self.move_to(self.len(), false),

            // ── Delete ──────────────────────────────────────────────────────
            Key::Backspace if plain => self.backspace(false),
            Key::Delete if plain => self.backspace(true),
            Key::Char('h') if ctrl => self.backspace(false),
            Key::Char('d') if ctrl => self.backspace(true),

            // ── Kill ────────────────────────────────────────────────────────
            Key::Char('u') if ctrl => self.kill(0),
            Key::Char('k') if ctrl => self.kill(self.len()),
            Key::Char('w') if ctrl => self.kill(self.backward()),
            Key::Char('d') if alt => self.kill(self.forward()),

            // ── Undo / redo ─────────────────────────────────────────────────
            Key::Char('z') if ctrl => self.undo(),
            Key::Char('z') if ctrl_shift => self.redo(),
            Key::Char('y') if ctrl => self.redo(),

            // Anything else is swallowed rather than leaked: a key with no
            // meaning in a prompt must not reach the file list behind it.
            _ => {}
        }
        InputEvent::Consumed
    }

    // ── Commands ────────────────────────────────────────────────────────────

    /// A printable key. A live selection is what it replaces.
    fn type_char(&mut self, c: char) {
        self.tag_once();
        let at = self.take_selection();
        self.splice(at, at, &c.to_string());
        self.cursor = at + 1;
    }

    /// `<Backspace>` / `<Delete>`, or the selection when there is one.
    fn backspace(&mut self, under: bool) {
        if self.selection().is_some() {
            self.tag();
            self.take_selection();
            return;
        }
        let len = self.len();
        let (from, to) = if under {
            if self.cursor >= len {
                return;
            }
            (self.cursor, self.cursor + 1)
        } else {
            if self.cursor == 0 {
                return;
            }
            (self.cursor - 1, self.cursor)
        };
        self.tag();
        self.splice(from, to, "");
        self.cursor = from;
    }

    /// The `kill` family: everything between the caret and `target`. A live
    /// selection wins, as it does for a plain delete.
    fn kill(&mut self, target: usize) {
        if self.selection().is_some() {
            self.tag();
            self.take_selection();
            return;
        }
        let target = target.min(self.len());
        let (lo, hi) = if target < self.cursor {
            (target, self.cursor)
        } else {
            (self.cursor, target)
        };
        if lo == hi {
            return;
        }
        self.tag();
        self.splice(lo, hi, "");
        self.cursor = lo;
    }

    // ── Motions ─────────────────────────────────────────────────────────────

    /// A motion landed on `target`: move there, extending the selection or
    /// dropping it.
    fn move_to(&mut self, target: usize, extend: bool) {
        let target = target.min(self.len());
        self.anchor = if extend {
            self.anchor.or(Some(self.cursor)).filter(|a| *a != target)
        } else {
            None
        };
        self.cursor = target;
    }

    /// One character left — or, with a selection live and no Shift held, the
    /// selection's near edge. Collapsing to the edge *is* the move, which is
    /// what every other text field does: Left after selecting a word puts the
    /// caret at the word's start rather than one character further back.
    fn left(&self, extend: bool) -> usize {
        match self.selection() {
            Some(range) if !extend => range.start,
            _ => self.cursor.saturating_sub(1),
        }
    }

    /// One character right, or the selection's far edge. See [`InputBuffer::left`].
    fn right(&self, extend: bool) -> usize {
        match self.selection() {
            Some(range) if !extend => range.end,
            _ => (self.cursor + 1).min(self.len()),
        }
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// One word back: the start of the word the caret is in, or of the
    /// previous one if it is already there.
    fn backward(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor.min(chars.len());
        if i == 0 {
            return 0;
        }
        i -= 1;
        while i > 0 && kind(chars[i]) == CharKind::Space {
            i -= 1;
        }
        if kind(chars[i]) == CharKind::Space {
            return i;
        }
        let k = kind(chars[i]);
        while i > 0 && kind(chars[i - 1]) == k {
            i -= 1;
        }
        i
    }

    /// One word forward: over any space under the caret, then past the end of
    /// the word after it — readline's `Alt+f`, which is also the span `Alt+d`
    /// deletes.
    fn forward(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let n = chars.len();
        let mut i = self.cursor;
        while i < n && kind(chars[i]) == CharKind::Space {
            i += 1;
        }
        if i >= n {
            return n;
        }
        let k = kind(chars[i]);
        while i < n && kind(chars[i]) == k {
            i += 1;
        }
        i
    }

    // ── Selection ───────────────────────────────────────────────────────────

    /// Remove the selection if there is one, and answer where the caret ends
    /// up: the range's start when something was removed, the caret's own
    /// position when there was nothing to remove.
    ///
    /// Every mutation goes through here, which is why "typing replaces the
    /// selection" is one rule and not five.
    fn take_selection(&mut self) -> usize {
        let Some(range) = self.selection() else {
            self.anchor = None;
            return self.cursor;
        };
        self.splice(range.start, range.end, "");
        self.anchor = None;
        self.cursor = range.start;
        range.start
    }

    // ── Undo ────────────────────────────────────────────────────────────────

    /// Record the line as it is *before* a mutation.
    fn tag(&mut self) {
        let snap = Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
        };
        if self.undo.last().map(|last| &last.text) == Some(&snap.text) {
            self.tagged = true;
            return;
        }
        self.undo.push(snap);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.tagged = true;
    }

    /// Record only if this is the first mutation of a typing run, so that
    /// typing a whole name is one `Ctrl+z` rather than twenty. This is the
    /// behavior that makes undo usable in a prompt at all.
    fn tag_once(&mut self) {
        if !self.tagged {
            self.tag();
        }
    }

    fn undo(&mut self) {
        let Some(prev) = self.undo.pop() else { return };
        self.redo.push(Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
        });
        if self.redo.len() > UNDO_LIMIT {
            self.redo.remove(0);
        }
        self.restore(prev);
    }

    fn redo(&mut self) {
        let Some(next) = self.redo.pop() else { return };
        self.undo.push(Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.restore(next);
    }

    /// A step of undo ends the typing run it stepped out of: the next
    /// character typed opens a new one, so a second `Ctrl+z` takes back the
    /// word before rather than the same word again.
    fn restore(&mut self, snap: Snapshot) {
        self.text = snap.text;
        self.tagged = false;
        self.cursor = snap.cursor.min(self.len());
        self.anchor = snap.anchor.filter(|a| *a <= self.len());
    }

    // ── Text plumbing ───────────────────────────────────────────────────────

    /// Byte offset of char index `i`, saturating at the end of the string.
    fn byte_of(&self, i: usize) -> usize {
        self.text
            .char_indices()
            .nth(i)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }

    /// Replace the char range `from..to` with `with`. The only function in this
    /// module that touches bytes, which is what keeps multibyte text safe: a
    /// caller can only ever name a char boundary.
    fn splice(&mut self, from: usize, to: usize, with: &str) {
        let (a, b) = (self.byte_of(from), self.byte_of(to));
        self.text.replace_range(a..b, with);
    }
}

/// The character a chord types, if it types one.
///
/// Shift is applied here rather than stored, per [`crate::keymap::Key`]'s rule
/// that a key is held unshifted: `Shift+,` is stored as `,` and types `<`.
/// `Tab` deliberately types nothing — it is completion, which belongs to the
/// prompt shell, not to the buffer.
fn printable(chord: Chord) -> Option<char> {
    match chord.key {
        Key::Space => Some(' '),
        Key::Char(c) if chord.mods.shift => chord.key.shifted_glyph().or(Some(c)),
        Key::Char(c) => Some(c),
        _ => None,
    }
}
