//! The vi line editor behind every prompt (PLAN §4.2).
//!
//! Rename, filter, create, `cd`, search and shell all put a single line of text
//! in front of the user, and in yazi they all get the same editor: a real modal
//! one, with `i I a A v r`, word motions, operators, a yank register and undo.
//! Porting it is not nostalgia — it is the difference between renaming
//! `2024-holiday-DSC_0198.jpeg` by holding Backspace and doing it with `cw`.
//!
//! This module is the whole editor as a **pure state machine**: chords in,
//! [`InputEvent`] out, and four accessors ([`InputBuffer::text`],
//! [`InputBuffer::cursor`], [`InputBuffer::selection`], [`InputBuffer::mode`])
//! that the UI paints from. It knows nothing about popups, prompts or where the
//! text is going, which is why it can be exercised exhaustively by `cargo test`
//! with no display attached (PLAN §1).
//!
//! # The authority is the keymap, not a memory of vim
//!
//! Every binding below is transcribed from the `[input]` section of the yazi
//! keymap on this machine, including the parts that are not vim:
//!
//! * **No counts.** yazi's input has no `3w`, no `d2w` — digits are either `0`
//!   (BOL) or unbound. We match that. A prompt is one filename long; a count is
//!   a feature nobody would reach for and every implementation would have to
//!   carry forever.
//! * **The three selection commands, and the quirk.** `V` runs
//!   `move bol → visual → move eol`, `<C-e>` runs the same, and `<C-A>` runs
//!   `move eol → visual → move bol`. All three select the entire line; they
//!   differ only in *which end the cursor lands on*, which is visible on screen
//!   and decides where a following operator leaves you. `<C-A>` (Ctrl+**Shift**+a)
//!   is a different binding from `<C-a>` (Ctrl+a, move-to-BOL); the shift bit is
//!   the only thing between them, which is exactly the normalization
//!   [`crate::keymap::Chord`] exists to make reliable.
//! * **Operators are pending, then a motion completes them.** `D` is spelled
//!   `delete --cut` + `move eol`, and `x` is `delete --cut` +
//!   `move 1 --in-operating`. That is not a shorthand in the config, it is how
//!   the editor works: `d` arms an operator and the next motion decides its
//!   range. So `dw`, `d$`, `db` all fall out for free, and `dd` (operator
//!   already armed) takes the whole line.
//!
//! # Ranges
//!
//! An operator driven by a motion takes a **half-open** range — `dw` deletes up
//! to but not including the first character of the next word, as in vim. An
//! operator applied to a **visual selection is inclusive** of the character
//! under the cursor, also as in vim. This is the one place the two differ and
//! the source of most off-by-one bugs in hand-rolled vi editors, so it is a
//! single flag (`include`) threaded through the operator's `fire`.
//!
//! # Unicode
//!
//! Positions are **char (Unicode scalar) indices**, not bytes and not extended
//! grapheme clusters. Bytes are wrong — `h` over `é` would split it and panic.
//! Graphemes would be righter still (a flag emoji is seven scalars and one
//! thing you point at), but they need a segmentation table that is a dependency
//! this crate will not take (PLAN §1), and the payload here is *filenames*,
//! where a combining sequence is rare and a family-emoji ZWJ chain is rarer.
//! Scalar boundaries never panic and never corrupt the string; they can only
//! cost an extra press on text almost nobody types. That trade is deliberate,
//! and it is the one thing in this module that is knowingly not verbatim.
//!
//! Word motions are class-based over `char::is_alphanumeric`, so `naïve_café`
//! is one word and `日本語` is one word — the multibyte case is handled by
//! asking the character what it is, never by counting bytes.

use crate::keymap::{Chord, InputMode, Key, Mods};

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

/// The editor's own mode, which is only ever three things.
///
/// Visual is deliberately **not** one of them: in yazi it is an armed `Select`
/// operator sitting on top of Normal, which is why `Esc` in a visual selection
/// drops the selection and leaves you in Normal rather than closing the prompt.
/// [`InputBuffer::mode`] reports [`InputMode::Visual`] to the UI when such an
/// operator is live, because the *badge* is a fourth mode even though the state
/// machine has three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Keys are commands.
    Normal,
    /// Keys are text.
    Insert,
    /// The next printable key overwrites the character under the cursor and
    /// drops back to Normal — the `r` in `i I a A v r`.
    Replace,
}

/// What a keystroke did to the prompt, from the caller's point of view.
///
/// There is no `Ignored`: an open prompt swallows every key it is handed, so
/// that a stray `q` while renaming types a `q` instead of quitting the
/// application. Unbound keys in Normal mode are consumed and do nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// The buffer handled it. Repaint from the accessors.
    Consumed,
    /// `Enter`: the user is done. Carries the final text.
    Submit(String),
    /// `Ctrl+c`, or `Esc` at the bottom of the ladder. Throw the prompt away.
    Cancel,
}

/// A saved line, for undo and redo.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    text: String,
    cursor: usize,
}

/// An armed operator waiting for a motion to give it a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// Visual mode: the anchor of the selection. Never completes; it is
    /// consumed by a following `d`/`c`/`y`/`x` or dropped by `Esc`.
    Select { anchor: usize },
    /// `d` / `c` / `s` / `S` / `D` / `C` / `x`, and the `kill` family.
    Delete {
        /// Whether the removed text lands in the yank register. The `backspace`
        /// family passes `false`; everything else `true`.
        cut: bool,
        /// `c` and friends drop into Insert mode once the range is gone.
        insert: bool,
        anchor: usize,
    },
    /// `y`.
    Yank { anchor: usize },
}

/// What class a character belongs to, for word motions.
///
/// `far` (yazi's `--far`, vim's WORD) collapses `Punct` into `Word`, which is
/// the whole difference between `w` and `W`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Space,
    Punct,
    Word,
}

fn kind(c: char, far: bool) -> CharKind {
    if c.is_whitespace() {
        CharKind::Space
    } else if far || c.is_alphanumeric() || c == '_' {
        CharKind::Word
    } else {
        CharKind::Punct
    }
}

/// One line of text being edited modally.
///
/// Construct with [`InputBuffer::new`] (or a rename preset), feed it
/// [`Chord`]s, paint from the accessors.
#[derive(Debug, Clone)]
pub struct InputBuffer {
    text: String,
    /// Char index. May equal the char length in Insert mode (the caret sits
    /// after the last character); in Normal mode it stops one short, as vim's
    /// block cursor must sit *on* something.
    cursor: usize,
    mode: Mode,
    op: Option<Op>,
    yank: String,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Set while an Insert run is in progress, so that a word of typing is one
    /// undo step rather than one per keystroke.
    tagged: bool,
    /// Whether `Esc` walks the modal ladder (Insert → Normal → cancel) or
    /// simply cancels — the `[input] vi_mode` switch, off by default.
    ///
    /// Everything else in this editor is live either way: `Ctrl+w`, `Ctrl+u`,
    /// the arrows and the word motions are all reachable from Insert. The flag
    /// buys back the *one* key whose vi meaning collides with what `Esc` means
    /// in every dialog ever drawn — "I did not mean to open this" — and pays
    /// for it with the mode the rest of the editor is entered through.
    vi: bool,
}

impl InputBuffer {
    /// A prompt over `initial`, with the caret at char index `cursor_pos`
    /// (clamped), in Insert mode.
    ///
    /// Insert is the right default because every prompt in the plan is opened
    /// to be typed into — filter, create, `cd`, search. Normal-mode entry would
    /// mean the first character of a filter query silently ran a command.
    pub fn new(initial: impl Into<String>, cursor_pos: usize) -> InputBuffer {
        let text = initial.into();
        let len = text.chars().count();
        InputBuffer {
            text,
            cursor: cursor_pos.min(len),
            mode: Mode::Insert,
            op: None,
            yank: String::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            tagged: false,
            vi: false,
        }
    }

    /// Turn the modal `Esc` ladder on (`[input] vi_mode = true`).
    ///
    /// A builder rather than a constructor argument because it is a *setting*,
    /// not a property of the prompt: every call site builds the buffer the same
    /// way and the app stamps the user's answer on it in one place
    /// (`App::open_prompt_with`).
    pub fn vi_mode(mut self, on: bool) -> InputBuffer {
        self.vi = on;
        self
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

    /// The mode to show in the prompt's badge.
    ///
    /// Reported as [`crate::keymap::InputMode`] — the same enum the keymap
    /// router uses to decide which bindings are live — so the badge and the
    /// dispatcher can never disagree about what mode the prompt is in.
    pub fn mode(&self) -> InputMode {
        match (self.mode, self.op) {
            (Mode::Normal, Some(Op::Select { .. })) => InputMode::Visual,
            (Mode::Normal, _) => InputMode::Normal,
            (Mode::Insert, _) => InputMode::Insert,
            (Mode::Replace, _) => InputMode::Replace,
        }
    }

    /// The live visual selection as a half-open **char** range, already
    /// inclusive of the character under the cursor. `None` when not in visual.
    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        let Some(Op::Select { anchor }) = self.op else {
            return None;
        };
        let len = self.len();
        let (lo, hi) = if anchor <= self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        Some(lo.min(len)..(hi + 1).min(len))
    }

    /// The same range in bytes, for slicing [`InputBuffer::text`] directly.
    pub fn selection_bytes(&self) -> Option<std::ops::Range<usize>> {
        let r = self.selection()?;
        Some(self.byte_of(r.start)..self.byte_of(r.end))
    }

    /// Whether an operator is armed and waiting for a motion — `d` pressed,
    /// `w` not yet. The UI can show it the way vim's ruler does; nothing is
    /// required to.
    pub fn has_pending_operator(&self) -> bool {
        matches!(self.op, Some(Op::Delete { .. }) | Some(Op::Yank { .. }))
    }

    /// The internal register `y`/`d`/`c`/`x`/kill fill and `p`/`P` read. It is
    /// deliberately *not* the system clipboard: `Y` in the file list is the
    /// clipboard key (PLAN §7.4), and a rename must not clobber what you copied
    /// out of a browser two minutes ago.
    pub fn yanked(&self) -> &str {
        &self.yank
    }

    // ── The dispatcher ──────────────────────────────────────────────────────

    /// Feed one keystroke.
    ///
    /// Takes a [`Chord`] rather than a bare [`Key`] because half of the
    /// `[input]` map is modified — `<C-a>` and `<C-A>` are different commands —
    /// and a `Key` alone cannot tell them apart.
    pub fn feed(&mut self, chord: Chord) -> InputEvent {
        // Text first: in Insert and Replace, an unmodified printable key is
        // *text*, which is why `i` types an `i` there and runs a command in
        // Normal. Modified chords fall through to the map, so `Ctrl+u` still
        // kills to BOL mid-word and the arrows still move.
        if self.mode != Mode::Normal && !chord.mods.ctrl && !chord.mods.alt && !chord.mods.super_key
        {
            if let Some(c) = printable(chord) {
                if self.mode == Mode::Replace {
                    self.replace_char(c);
                } else {
                    self.type_char(c);
                }
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
    /// Semantics follow the mode, and match typing the characters one at a
    /// time:
    ///
    /// * **Insert** — inserted at the caret, which ends after them.
    /// * **Replace** — overwrites forward, one character of the buffer per
    ///   character committed, stopping at the end of the line rather than
    ///   extending it (Replace never changes the line's length, which is what
    ///   makes `r` at the end of an empty line a no-op); then back to Normal
    ///   with the caret on the last character written, as `r` leaves it.
    /// * **Normal** — ignored. Text only reaches a program while something is
    ///   *composing*, and in Normal mode keys are commands, so there is nothing
    ///   composing and this call is a mode error somewhere upstream. Swallowing
    ///   it is the conservative half of the choice: the alternative — treating
    ///   the commit as text — would let an IME that fires one frame late type
    ///   `d` into a line the user thought they were deleting from.
    ///
    /// Undo-coalesced like consecutive insert keys: a commit lands inside the
    /// Insert run it arrives in, so `u` takes back the whole word rather than
    /// splitting it around the composition.
    ///
    /// The text is taken verbatim, newlines and all — this buffer is a string,
    /// not a filename validator, and a caller that does not want the trailing
    /// `\n` off a clipboard should trim it before it gets here.
    pub fn insert_text(&mut self, text: &str) -> InputEvent {
        if text.is_empty() {
            return InputEvent::Consumed;
        }
        match self.mode {
            Mode::Normal => InputEvent::Consumed,
            Mode::Insert => {
                self.tag_once();
                let at = self.cursor;
                self.splice(at, at, text);
                self.cursor = at + text.chars().count();
                InputEvent::Consumed
            }
            Mode::Replace => {
                let at = self.cursor;
                // Replace never changes the length of the line — that is the
                // invariant `r` has, and "replace one character at the end of
                // an empty line does nothing" is the same rule read at the
                // boundary. A commit longer than what is left of the line
                // overwrites what it can and drops the overflow rather than
                // growing the line behind an `r`.
                let room = self.len().saturating_sub(at);
                let n = text.chars().count().min(room);
                if n > 0 {
                    self.tag();
                    let written: String = text.chars().take(n).collect();
                    self.splice(at, at + n, &written);
                }
                self.mode = Mode::Normal;
                self.cursor = (at + n.saturating_sub(1)).min(self.limit());
                InputEvent::Consumed
            }
        }
    }

    /// The `[input]` keymap, transcribed.
    fn command(&mut self, chord: Chord) -> InputEvent {
        let m = chord.mods;
        let plain = m.is_none();
        let shift = m == Mods::SHIFT;
        let ctrl = m == Mods::CTRL;
        // `<C-A>`: Ctrl held *and* Shift held. The one binding that needs both.
        let ctrl_shift = m.ctrl && m.shift && !m.alt && !m.super_key;
        let alt = m == Mods::ALT;

        match chord.key {
            // ── Close ───────────────────────────────────────────────────────
            Key::Char('c') if ctrl => return InputEvent::Cancel,
            Key::Enter if plain => return InputEvent::Submit(self.text.clone()),
            // `<Esc>` and `<C-[>` are one command in the keymap because they
            // are one keystroke on a terminal; here they are two chords that
            // must still behave identically.
            Key::Escape if plain => return self.escape(),
            Key::Char('[') if ctrl => return self.escape(),

            // ── Mode ────────────────────────────────────────────────────────
            Key::Char('i') if plain => self.insert(false),
            Key::Char('i') if shift => {
                // `I`: move first-char, insert.
                self.motion_to(self.first_char());
                self.insert(false);
            }
            Key::Char('a') if plain => self.insert(true),
            Key::Char('a') if shift => {
                // `A`: move eol, insert --append.
                self.motion_to(self.eol());
                self.insert(true);
            }
            Key::Char('v') if plain => self.visual(),
            Key::Char('r') if plain => self.mode = Mode::Replace,

            // ── Selection ───────────────────────────────────────────────────
            // `V` and `<C-e>` are BOL→EOL; `<C-A>` is EOL→BOL. Same span, and
            // the cursor deliberately ends at opposite ends. See the header.
            Key::Char('v') if shift => self.select_line(false),
            Key::Char('e') if ctrl => self.select_line(false),
            Key::Char('a') if ctrl_shift => self.select_line(true),

            // ── Character-wise movement ─────────────────────────────────────
            Key::Char('h') if plain => self.motion_to(self.cursor.saturating_sub(1)),
            Key::Char('l') if plain => self.motion_to(self.cursor + 1),
            Key::ArrowLeft if plain => self.motion_to(self.cursor.saturating_sub(1)),
            Key::ArrowRight if plain => self.motion_to(self.cursor + 1),
            Key::Char('b') if ctrl => self.motion_to(self.cursor.saturating_sub(1)),
            Key::Char('f') if ctrl => self.motion_to(self.cursor + 1),

            // ── Word-wise movement ──────────────────────────────────────────
            Key::Char('b') if plain => self.motion_to(self.backward(false)),
            Key::Char('b') if shift => self.motion_to(self.backward(true)),
            Key::Char('w') if plain => self.motion_to(self.forward(false)),
            Key::Char('w') if shift => self.motion_to(self.forward(true)),
            Key::Char('e') if plain => self.motion_to(self.end_of_word(false)),
            Key::Char('e') if shift => self.motion_to(self.end_of_word(true)),
            Key::Char('b') if alt => self.motion_to(self.backward(false)),
            Key::Char('f') if alt => self.motion_to(self.end_of_word(false)),

            // ── Line-wise movement ──────────────────────────────────────────
            Key::Char('0') if plain => self.motion_to(0),
            // `$` `^` `_` are shifted keys: `$` is Shift+4, `^` is Shift+6,
            // `_` is Shift+-. Storing them unshifted is what keymap::Key is
            // for; matching them means naming the base key plus Shift.
            Key::Char('4') if shift => self.motion_to(self.eol()),
            Key::Char('6') if shift => self.motion_to(self.first_char()),
            Key::Char('-') if shift => self.motion_to(self.first_char()),
            Key::Char('a') if ctrl => self.motion_to(0),
            Key::Home if plain => self.motion_to(0),
            Key::End if plain => self.motion_to(self.eol()),

            // ── Delete ──────────────────────────────────────────────────────
            Key::Backspace if plain => self.backspace(false),
            Key::Delete if plain => self.backspace(true),
            Key::Char('h') if ctrl => self.backspace(false),
            Key::Char('d') if ctrl => self.backspace(true),

            // ── Kill ────────────────────────────────────────────────────────
            Key::Char('u') if ctrl => self.kill(0),
            Key::Char('k') if ctrl => self.kill(self.len()),
            Key::Char('w') if ctrl => self.kill(self.backward(false)),
            // "kill forwards to the *end* of the current word" — inclusive of
            // that last character, which is what readline's Alt+d does too, so
            // the target is one past `e`.
            Key::Char('d') if alt => self.kill((self.end_of_word(false) + 1).min(self.len())),

            // ── Cut / yank / paste ──────────────────────────────────────────
            Key::Char('d') if plain => self.delete(true, false),
            Key::Char('d') if shift => {
                self.delete(true, false);
                self.motion_to(self.eol());
            }
            Key::Char('c') if plain => self.delete(true, true),
            Key::Char('c') if shift => {
                self.delete(true, true);
                self.motion_to(self.eol());
            }
            Key::Char('s') if plain => {
                self.delete(true, true);
                self.motion_to(self.cursor + 1);
            }
            Key::Char('s') if shift => {
                self.motion_to(0);
                self.delete(true, true);
                self.motion_to(self.eol());
            }
            Key::Char('x') if plain => {
                self.delete(true, false);
                // `move 1 --in-operating`: the move exists only to close the
                // operator, so it must not run if the operator already fired
                // (which it does when there was a visual selection).
                if self.has_pending_operator() {
                    self.motion_to(self.cursor + 1);
                }
            }
            Key::Char('y') if plain => self.yank(),
            Key::Char('p') if plain => self.paste(false),
            Key::Char('p') if shift => self.paste(true),

            // ── Undo / redo ─────────────────────────────────────────────────
            Key::Char('u') if plain => self.undo(),
            Key::Char('r') if ctrl => self.redo(),

            // `~` and `<F1>` open yazi's help overlay, which is the shell's
            // business, not the buffer's: it is a separate keymap context
            // (PLAN §4). Consumed here so the key never leaks into the text.
            _ => {}
        }
        InputEvent::Consumed
    }

    // ── Commands ────────────────────────────────────────────────────────────

    /// `Esc` **cancels the prompt**, first press.
    ///
    /// That is what the key means everywhere else in the program and everywhere
    /// else on the desktop: the thing I opened, close it. yazi's ladder — drop
    /// the operator, step Insert back into Normal, and only *then* close —
    /// spends two presses on getting out of a prompt somebody opened by
    /// mistake, and leaves a block caret behind on the first one, which reads
    /// as the prompt having broken rather than as a mode having been entered.
    ///
    /// The ladder is still there behind `[input] vi_mode = true`
    /// ([`InputBuffer::vi_mode`]), verbatim: an operator or a selection is
    /// dropped first, Insert steps back into Normal, and only a bare
    /// Normal-mode `Esc` closes the prompt.
    fn escape(&mut self) -> InputEvent {
        if !self.vi {
            return InputEvent::Cancel;
        }
        match self.mode {
            Mode::Insert | Mode::Replace => {
                self.mode = Mode::Normal;
                self.tagged = false;
                // Leaving insert steps the caret back onto the last character
                // typed, as in vim; the block cursor cannot sit past the end.
                self.cursor = self.cursor.saturating_sub(1).min(self.limit());
                InputEvent::Consumed
            }
            Mode::Normal if self.op.is_some() => {
                self.op = None;
                self.cursor = self.cursor.min(self.limit());
                InputEvent::Consumed
            }
            Mode::Normal => InputEvent::Cancel,
        }
    }

    fn insert(&mut self, append: bool) {
        self.op = None;
        self.tag();
        self.mode = Mode::Insert;
        if append {
            self.cursor = (self.cursor + 1).min(self.len());
        }
    }

    fn visual(&mut self) {
        self.op = Some(Op::Select {
            anchor: self.cursor,
        });
    }

    /// `V` / `<C-e>` (BOL→EOL) and `<C-A>` (EOL→BOL).
    fn select_line(&mut self, backwards: bool) {
        let last = self.limit();
        if backwards {
            self.cursor = last;
            self.visual();
            self.cursor = 0;
        } else {
            self.cursor = 0;
            self.visual();
            self.cursor = last;
        }
    }

    /// Arm a delete, or fire it at the current selection.
    fn delete(&mut self, cut: bool, insert: bool) {
        match self.op {
            Some(Op::Select { anchor }) => {
                self.op = Some(Op::Delete {
                    cut,
                    insert,
                    anchor,
                });
                self.fire(self.cursor, true);
            }
            // `dd`: the operator is already armed, so the object is the line.
            Some(Op::Delete { .. }) => {
                self.op = Some(Op::Delete {
                    cut,
                    insert,
                    anchor: 0,
                });
                self.fire(self.len(), false);
            }
            _ => {
                self.op = Some(Op::Delete {
                    cut,
                    insert,
                    anchor: self.cursor,
                })
            }
        }
    }

    fn yank(&mut self) {
        match self.op {
            Some(Op::Select { anchor }) => {
                self.op = Some(Op::Yank { anchor });
                self.fire(self.cursor, true);
            }
            // `yy`.
            Some(Op::Yank { .. }) => {
                self.op = Some(Op::Yank { anchor: 0 });
                self.fire(self.len(), false);
            }
            _ => {
                self.op = Some(Op::Yank {
                    anchor: self.cursor,
                })
            }
        }
    }

    /// The `kill` family: a cut from the caret to `target`, half-open.
    fn kill(&mut self, target: usize) {
        self.op = Some(Op::Delete {
            cut: true,
            insert: false,
            anchor: self.cursor,
        });
        self.fire(target, false);
    }

    /// `<Backspace>` / `<Delete>`. Not a cut: readline's Backspace has never
    /// filled a register and a rename that clobbered the yank on a typo would
    /// be a small betrayal.
    fn backspace(&mut self, under: bool) {
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
        self.cursor = from.min(self.limit());
    }

    /// `p` (after the caret) and `P` (before it). A live selection is replaced,
    /// as in vim.
    fn paste(&mut self, before: bool) {
        if self.yank.is_empty() {
            return;
        }
        let text = self.yank.clone();
        let n = text.chars().count();
        self.tag();
        let at = match self.selection() {
            Some(r) => {
                self.splice(r.start, r.end, "");
                self.op = None;
                r.start
            }
            None if before => self.cursor,
            None => (self.cursor + 1).min(self.len()),
        };
        self.splice(at, at, &text);
        self.cursor = (at + n).saturating_sub(1).min(self.limit());
    }

    /// A printable key in Insert mode.
    fn type_char(&mut self, c: char) {
        self.tag_once();
        let at = self.cursor;
        self.splice(at, at, &c.to_string());
        self.cursor = at + 1;
    }

    /// A printable key in Replace mode: one character, then back to Normal.
    fn replace_char(&mut self, c: char) {
        self.tag();
        let at = self.cursor;
        if at < self.len() {
            self.splice(at, at + 1, &c.to_string());
        }
        self.mode = Mode::Normal;
        self.cursor = at.min(self.limit());
    }

    // ── Motions ─────────────────────────────────────────────────────────────

    /// A motion landed on `target`: either it closes an armed operator, or it
    /// just moves the caret (extending the selection, if there is one).
    fn motion_to(&mut self, target: usize) {
        let target = target.min(self.len());
        if self.has_pending_operator() {
            self.fire(target, false);
        } else {
            self.cursor = target.min(self.limit());
        }
    }

    /// The furthest the caret may sit: past the last character in Insert (a
    /// caret between characters), on it in Normal (a block on a character).
    fn limit(&self) -> usize {
        match self.mode {
            Mode::Normal => self.len().saturating_sub(1),
            _ => self.len(),
        }
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// `move eol`: one past the last character.
    ///
    /// Motions compute *raw* targets and [`InputBuffer::motion_to`] clamps only
    /// when the target is going to become a caret. That single rule is why `$`
    /// lands **on** the last character in Normal mode while `d$` still takes it
    /// — the same motion, clamped in one path and not the other.
    fn eol(&self) -> usize {
        self.len()
    }

    /// `move first-char`: the first non-whitespace character, or BOL if the
    /// line is blank.
    fn first_char(&self) -> usize {
        self.text
            .chars()
            .position(|c| !c.is_whitespace())
            .unwrap_or(0)
    }

    /// `b` / `B`: the start of the current word, or of the previous one if
    /// already there.
    fn backward(&self, far: bool) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor.min(chars.len());
        if i == 0 {
            return 0;
        }
        i -= 1;
        while i > 0 && kind(chars[i], far) == CharKind::Space {
            i -= 1;
        }
        if kind(chars[i], far) == CharKind::Space {
            return i;
        }
        let k = kind(chars[i], far);
        while i > 0 && kind(chars[i - 1], far) == k {
            i -= 1;
        }
        i
    }

    /// `w` / `W`: the start of the next word.
    fn forward(&self, far: bool) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let n = chars.len();
        let mut i = self.cursor;
        if i >= n {
            return n;
        }
        let k = kind(chars[i], far);
        if k != CharKind::Space {
            while i < n && kind(chars[i], far) == k {
                i += 1;
            }
        }
        while i < n && kind(chars[i], far) == CharKind::Space {
            i += 1;
        }
        i
    }

    /// `e` / `E`: the last character of the current word, or of the next one if
    /// already on it.
    fn end_of_word(&self, far: bool) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let n = chars.len();
        if n == 0 {
            return 0;
        }
        let mut i = self.cursor + 1;
        while i < n && kind(chars[i], far) == CharKind::Space {
            i += 1;
        }
        if i >= n {
            return n - 1;
        }
        let k = kind(chars[i], far);
        while i + 1 < n && kind(chars[i + 1], far) == k {
            i += 1;
        }
        i
    }

    // ── Operator application ────────────────────────────────────────────────

    /// Close the armed operator over the span between its anchor and `to`.
    ///
    /// `include` is the half-open/inclusive switch from the module header: a
    /// motion-driven operator passes `false` (`dw` stops short of the next
    /// word), a visual-driven one passes `true` (the character under the block
    /// cursor is part of what you can see is selected).
    fn fire(&mut self, to: usize, include: bool) {
        let Some(op) = self.op.take() else { return };
        let (cut, insert, anchor) = match op {
            Op::Delete {
                cut,
                insert,
                anchor,
            } => (cut, insert, anchor),
            Op::Yank { anchor } => {
                let (lo, hi) = span(anchor, to, include, self.len());
                self.yank = self.slice(lo, hi);
                self.cursor = lo.min(self.limit());
                return;
            }
            Op::Select { .. } => return,
        };
        let (lo, hi) = span(anchor, to, include, self.len());
        if lo == hi && !insert {
            self.cursor = lo.min(self.limit());
            return;
        }
        self.tag();
        // An empty cut (`c` on a zero-width span) must not wipe the register:
        // the user asked to change nothing, not to forget what they copied.
        if cut && hi > lo {
            self.yank = self.slice(lo, hi);
        }
        self.splice(lo, hi, "");
        if insert {
            self.mode = Mode::Insert;
            self.tagged = true;
        }
        self.cursor = lo.min(self.limit());
    }

    // ── Undo ────────────────────────────────────────────────────────────────

    /// Record the line as it is *before* a mutation.
    fn tag(&mut self) {
        let snap = Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
        };
        if self.undo.last() == Some(&snap) {
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

    /// Record only if this is the first mutation of an Insert run, so that
    /// typing a whole name is one `u` rather than twenty. This is the behavior
    /// that makes undo usable in a prompt at all.
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
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.restore(next);
    }

    /// Undo and redo both land you in Normal mode with nothing armed: the
    /// state you are stepping back to was a *committed* one, and re-entering it
    /// half inside an operator would be a state that never existed.
    fn restore(&mut self, snap: Snapshot) {
        self.text = snap.text;
        self.mode = Mode::Normal;
        self.op = None;
        self.tagged = false;
        self.cursor = snap.cursor.min(self.limit());
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

    fn slice(&self, from: usize, to: usize) -> String {
        self.text[self.byte_of(from)..self.byte_of(to)].to_string()
    }

    /// Replace the char range `from..to` with `with`. The only function in this
    /// module that touches bytes, which is what keeps multibyte text safe: a
    /// caller can only ever name a char boundary.
    fn splice(&mut self, from: usize, to: usize, with: &str) {
        let (a, b) = (self.byte_of(from), self.byte_of(to));
        self.text.replace_range(a..b, with);
    }
}

/// The span an operator covers, given its anchor, where the motion landed, and
/// whether the far end is inclusive.
fn span(anchor: usize, to: usize, include: bool, len: usize) -> (usize, usize) {
    let (lo, hi) = if anchor <= to {
        (anchor, to)
    } else {
        (to, anchor)
    };
    let hi = if include { hi + 1 } else { hi };
    (lo.min(len), hi.min(len))
}

/// The character a chord types, if it types one.
///
/// Shift is applied here rather than stored, per [`crate::keymap::Key`]'s rule
/// that a key is held unshifted: `Shift+,` is stored as `,` and types `<`.
/// `Tab` deliberately types nothing — in yazi it is completion, which belongs
/// to the prompt shell, not to the buffer.
fn printable(chord: Chord) -> Option<char> {
    match chord.key {
        Key::Space => Some(' '),
        Key::Char(c) if chord.mods.shift => chord.key.shifted_glyph().or(Some(c)),
        Key::Char(c) => Some(c),
        _ => None,
    }
}
