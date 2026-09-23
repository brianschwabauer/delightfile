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
//! already answers to and the one a shell user has in their fingers. They live
//! here as the **fallback** map ([`InputBuffer::binding`]): the keymap's
//! `[input]` table is consulted first by whoever owns the prompt, and what it
//! names arrives as an [`InputOp`] through [`InputBuffer::apply`]. Both roads
//! lead to the same verbs, which is the point — there is one editor, and the
//! table decides only which key reaches which part of it.
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
//!
//! # The pointer
//!
//! A click, a drag and a double click land here as the same verbs the keys
//! use, with the geometry already done by whoever drew the line: the caller
//! turns a pixel into a char index and hands over the index. A click is
//! [`InputBuffer::move_to`], a drag or a Shift+click is the same motion with
//! the selection extended, and a double or triple click is an explicit
//! [`InputBuffer::set_selection`] over the run [`segment_at`] finds. Nothing
//! here knows what a pixel is, which is what keeps it testable.

use crate::keymap::{Chord, Command, Key, Mods};

#[cfg(test)]
mod tests;

/// The action a keymap command asks the prompt for, if it asks for one.
///
/// This is what makes the `[input]` table real rather than decorative: the app
/// resolves a chord against the registry's `Input` context, brings the
/// [`Command`] here, and runs the answer. A command the prompt has no verb for
/// — anything somebody puts in an `[input]` table that is not an editing key —
/// answers `None`, and the chord falls through to the built-in readline map
/// where it is either text or nothing.
pub fn action_of(command: Command) -> Option<InputAction> {
    use Command as C;
    use InputAction::{Cancel, Edit, Submit};
    use InputOp as O;
    let action = match command {
        C::Escape | C::OverlayClose => Cancel,
        C::OverlaySubmit => Submit,
        C::InputMoveLeft => Edit(O::MoveLeft),
        C::InputMoveRight => Edit(O::MoveRight),
        C::InputMoveBol => Edit(O::MoveBol),
        C::InputMoveEol => Edit(O::MoveEol),
        C::InputWordForward => Edit(O::WordForward),
        C::InputWordBackward => Edit(O::WordBackward),
        C::InputSelectLeft => Edit(O::SelectLeft),
        C::InputSelectRight => Edit(O::SelectRight),
        C::InputSelectBol => Edit(O::SelectBol),
        C::InputSelectEol => Edit(O::SelectEol),
        C::InputSelectWordForward => Edit(O::SelectWordForward),
        C::InputSelectWordBackward => Edit(O::SelectWordBackward),
        C::InputBackspace => Edit(O::Backspace),
        C::InputDeleteUnder => Edit(O::DeleteUnder),
        C::InputKillBol => Edit(O::KillBol),
        C::InputKillEol => Edit(O::KillEol),
        C::InputKillWordBackward => Edit(O::KillWordBackward),
        C::InputKillWordForward => Edit(O::KillWordForward),
        C::InputUndo => Edit(O::Undo),
        C::InputRedo => Edit(O::Redo),
        _ => return None,
    };
    Some(action)
}

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

/// One editing verb, named apart from the key that runs it.
///
/// The keymap's `[input]` table names these — `input-kill-bol` and the rest —
/// and so does the built-in readline map below. Splitting the verb from the
/// chord is what lets a `keymap.toml` line rebind an editing key at all: the
/// app resolves the chord against the registry first and hands the answer
/// here, and only a chord the table has no row for falls back to
/// [`InputBuffer::binding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOp {
    MoveLeft,
    MoveRight,
    MoveBol,
    MoveEol,
    WordForward,
    WordBackward,
    SelectLeft,
    SelectRight,
    SelectBol,
    SelectEol,
    SelectWordForward,
    SelectWordBackward,
    Backspace,
    DeleteUnder,
    KillBol,
    KillEol,
    KillWordBackward,
    KillWordForward,
    Undo,
    Redo,
}

/// What one chord means to the editor: an edit, or one of the two answers that
/// end the prompt.
///
/// `None` from [`InputBuffer::binding`] is "the editor has no opinion", which
/// is what makes a printable key text and what lets the help sheet keep the
/// keys its filter does not want (see the app's `help_key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    Edit(InputOp),
    Submit,
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
    } else if is_word_char(c) {
        CharKind::Word
    } else {
        CharKind::Punct
    }
}

/// Whether `c` is part of a word, as the word motions (`Ctrl+←/→`, `Alt+b/f`,
/// `Ctrl+w`) read one: a letter or a digit in any script, or `_`.
///
/// Public so that a double click can select exactly the run those keys step
/// over: a caller hands `|c| !is_word_char(c)` to [`segment_at`] as its
/// separator test, and the word under the pointer is the word under the
/// caret.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The run of non-separator characters around char index `at`: what a double
/// click on `at` selects.
///
/// `at` is the character the pointer is *on*, not a caret position between
/// two of them — a click on the right half of the `n` in `brian` is on the
/// `n`, and asking for the boundary after it would select whatever follows
/// the `/`. An index at or past the end is the last character, so a double
/// click beyond the end of the line takes its last segment rather than
/// nothing.
///
/// A separator is not a segment of its own: a double click on one selects the
/// segment to its **right**, which is the one a path's `/` introduces. With
/// nothing but separators to the right (`/home/brian/`, clicked on the last
/// `/`), the segment to the left is taken instead, so the gesture always
/// selects something while there is anything to select. A line of nothing
/// but separators — or no line at all — answers the empty range at `at`.
///
/// ```text
/// / h o m e / b r i a n / D o w n l o a d s
/// 0 1 2 3 4 5 6 7 8 9 10 11 …
/// segment_at(_, 7, |c| c == '/') == 6..11   // "brian"
/// segment_at(_, 5, |c| c == '/') == 6..11   // the `/` before it
/// ```
pub fn segment_at(
    text: &str,
    at: usize,
    is_separator: impl Fn(char) -> bool,
) -> std::ops::Range<usize> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return 0..0;
    }
    let mut i = at.min(n - 1);
    if is_separator(chars[i]) {
        let right = (i..n).find(|&j| !is_separator(chars[j]));
        let left = || (0..i).rev().find(|&j| !is_separator(chars[j]));
        match right.or_else(left) {
            Some(j) => i = j,
            None => {
                let at = at.min(n);
                return at..at;
            }
        }
    }
    let start = (0..i)
        .rev()
        .find(|&j| is_separator(chars[j]))
        .map_or(0, |j| j + 1);
    let end = (i..n).find(|&j| is_separator(chars[j])).unwrap_or(n);
    start..end
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

    /// The keymap, transcribed — the **fallback** map, for a chord the
    /// registry's `[input]` table has no row for.
    ///
    /// Pure and associated rather than a method, so a caller can ask what a
    /// chord *would* do without a buffer to do it to. The help sheet asks
    /// exactly that: while its filter is open, a chord this answers for belongs
    /// to the field and must not also page the list behind it.
    pub fn binding(chord: Chord) -> Option<InputAction> {
        let m = chord.mods;
        let plain = m.is_none();
        let shift = m == Mods::SHIFT;
        let ctrl = m == Mods::CTRL;
        let ctrl_shift = m.ctrl && m.shift && !m.alt && !m.super_key;
        let alt = m == Mods::ALT;
        use InputAction::{Cancel, Edit, Submit};
        use InputOp as O;

        let action = match chord.key {
            // ── Close ───────────────────────────────────────────────────────
            Key::Char('c') if ctrl => Cancel,
            Key::Enter if plain => Submit,
            // `Esc` means "close this" here and in every other dialog in the
            // program, on the first press. `<C-[>` is the same keystroke on a
            // terminal and stays the same command here.
            Key::Escape if plain => Cancel,
            Key::Char('[') if ctrl => Cancel,

            // ── Word-wise movement ──────────────────────────────────────────
            // Before the character arrows, because `Ctrl+Left` is a word and
            // the bare arrow's arm would otherwise have to exclude it.
            Key::ArrowLeft if ctrl_shift => Edit(O::SelectWordBackward),
            Key::ArrowRight if ctrl_shift => Edit(O::SelectWordForward),
            Key::ArrowLeft if ctrl => Edit(O::WordBackward),
            Key::ArrowRight if ctrl => Edit(O::WordForward),
            Key::Char('b') if alt => Edit(O::WordBackward),
            Key::Char('f') if alt => Edit(O::WordForward),

            // ── Character-wise movement ─────────────────────────────────────
            Key::ArrowLeft if shift => Edit(O::SelectLeft),
            Key::ArrowRight if shift => Edit(O::SelectRight),
            Key::ArrowLeft if plain => Edit(O::MoveLeft),
            Key::ArrowRight if plain => Edit(O::MoveRight),
            Key::Char('b') if ctrl => Edit(O::MoveLeft),
            Key::Char('f') if ctrl => Edit(O::MoveRight),

            // ── Line-wise movement ──────────────────────────────────────────
            Key::Home if shift => Edit(O::SelectBol),
            Key::End if shift => Edit(O::SelectEol),
            Key::Home if plain => Edit(O::MoveBol),
            Key::End if plain => Edit(O::MoveEol),
            Key::Char('a') if ctrl => Edit(O::MoveBol),
            Key::Char('e') if ctrl => Edit(O::MoveEol),

            // ── Delete ──────────────────────────────────────────────────────
            Key::Backspace if plain => Edit(O::Backspace),
            Key::Delete if plain => Edit(O::DeleteUnder),
            Key::Char('h') if ctrl => Edit(O::Backspace),
            Key::Char('d') if ctrl => Edit(O::DeleteUnder),

            // ── Kill ────────────────────────────────────────────────────────
            Key::Char('u') if ctrl => Edit(O::KillBol),
            Key::Char('k') if ctrl => Edit(O::KillEol),
            Key::Char('w') if ctrl => Edit(O::KillWordBackward),
            Key::Char('d') if alt => Edit(O::KillWordForward),

            // ── Undo / redo ─────────────────────────────────────────────────
            Key::Char('z') if ctrl => Edit(O::Undo),
            Key::Char('z') if ctrl_shift => Edit(O::Redo),
            Key::Char('y') if ctrl => Edit(O::Redo),

            _ => return None,
        };
        Some(action)
    }

    /// Run one editing verb. The door the keymap's `[input]` rows come in
    /// through.
    pub fn apply(&mut self, op: InputOp) -> InputEvent {
        use InputOp as O;
        match op {
            O::MoveLeft => self.move_to(self.left(false), false),
            O::MoveRight => self.move_to(self.right(false), false),
            O::MoveBol => self.move_to(0, false),
            O::MoveEol => self.move_to(self.len(), false),
            O::WordBackward => self.move_to(self.backward(), false),
            O::WordForward => self.move_to(self.forward(), false),
            O::SelectLeft => self.move_to(self.left(true), true),
            O::SelectRight => self.move_to(self.right(true), true),
            O::SelectBol => self.move_to(0, true),
            O::SelectEol => self.move_to(self.len(), true),
            O::SelectWordBackward => self.move_to(self.backward(), true),
            O::SelectWordForward => self.move_to(self.forward(), true),
            O::Backspace => self.backspace(false),
            O::DeleteUnder => self.backspace(true),
            O::KillBol => self.kill(0),
            O::KillEol => self.kill(self.len()),
            O::KillWordBackward => self.kill(self.backward()),
            O::KillWordForward => self.kill(self.forward()),
            O::Undo => self.undo(),
            O::Redo => self.redo(),
        }
        InputEvent::Consumed
    }

    /// Run one resolved action — an edit, or the answer that ends the prompt.
    pub fn act(&mut self, action: InputAction) -> InputEvent {
        match action {
            InputAction::Edit(op) => self.apply(op),
            InputAction::Submit => InputEvent::Submit(self.text.clone()),
            InputAction::Cancel => InputEvent::Cancel,
        }
    }

    /// The built-in map, run.
    fn command(&mut self, chord: Chord) -> InputEvent {
        match InputBuffer::binding(chord) {
            Some(action) => self.act(action),
            // Anything else is swallowed rather than leaked: a key with no
            // meaning in a prompt must not reach the file list behind it.
            None => InputEvent::Consumed,
        }
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
    ///
    /// Every key motion ends here, and so does the pointer: a click is this
    /// with `extend` false, and a Shift+click or a drag from a press is this
    /// with `extend` true. An extended motion keeps the anchor a selection
    /// already has, or drops one at the caret if there is none — so a drag
    /// that began with a click is anchored where the click put the caret, and
    /// a Shift+click after a keyboard selection grows that selection rather
    /// than starting another. `target` is clamped to the line.
    pub fn move_to(&mut self, target: usize, extend: bool) {
        let target = target.min(self.len());
        self.anchor = if extend {
            self.anchor.or(Some(self.cursor)).filter(|a| *a != target)
        } else {
            None
        };
        self.cursor = target;
    }

    /// Select from `anchor` to `cursor`, caret at `cursor` — what a double or
    /// a triple click does, where the run is decided by what was clicked
    /// rather than grown from where the caret was.
    ///
    /// Both ends are clamped to the line, and an empty range is no selection
    /// with the caret at `cursor`, by the same rule as [`InputBuffer::selection`].
    pub fn set_selection(&mut self, anchor: usize, cursor: usize) {
        let len = self.len();
        let (anchor, cursor) = (anchor.min(len), cursor.min(len));
        self.anchor = (anchor != cursor).then_some(anchor);
        self.cursor = cursor;
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
