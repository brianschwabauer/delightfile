//! The one-line text buffer behind the bottom input bar — `f` filter, `/` and
//! `?` find, and the help browser's own filter field.
//!
//! PLAN §4.2 wants **one** input implementation shared by rename, filter,
//! create, cd, search and shell, with the full vi line editor on top of it. That
//! editor is a Phase 2 checkbox; what is here is the buffer it will be built
//! *on* — a byte-offset cursor over a `String` with the edit primitives the
//! `[input]` bindings already name ([`Command::InputKillBol`] and friends), so
//! Phase 2 adds modes and motions to this file rather than replacing it.
//!
//! Everything is a pure operation on `(&mut Line)` with no clock and no egui, so
//! the awkward half — cursor arithmetic that has to land on character
//! boundaries, in a program whose whole test corpus is gnarly unicode filenames
//! (PLAN §9) — is unit-tested without a window.
//!
//! [`Command::InputKillBol`]: df_core::keymap::Command::InputKillBol

/// A single line of text and where the caret is in it.
///
/// The caret is a **byte** offset, always on a character boundary. Byte rather
/// than char because every consumer — `str` slicing, egui's `LayoutJob`
/// sections, df-core's [`Span`](df_core::fs::Span) match ranges — speaks bytes,
/// and converting at each of them is three chances to be off by one on a name
/// with an emoji in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    text: String,
    caret: usize,
}

impl Line {
    pub fn new() -> Line {
        Line::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Type something. Winit hands over a whole string rather than a character
    /// because one keystroke can produce several (a composed key), and pasting
    /// later arrives the same way.
    pub fn insert(&mut self, text: &str) {
        self.text.insert_str(self.caret, text);
        self.caret += text.len();
    }

    /// `Backspace` — delete the character *before* the caret.
    pub fn backspace(&mut self) {
        let Some(prev) = self.prev_boundary() else {
            return;
        };
        self.text.replace_range(prev..self.caret, "");
        self.caret = prev;
    }

    /// `Delete` — the character *under* the caret.
    pub fn delete_under(&mut self) {
        let Some(next) = self.next_boundary() else {
            return;
        };
        self.text.replace_range(self.caret..next, "");
    }

    pub fn move_left(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.caret = prev;
        }
    }

    pub fn move_right(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.caret = next;
        }
    }

    pub fn move_bol(&mut self) {
        self.caret = 0;
    }

    pub fn move_eol(&mut self) {
        self.caret = self.text.len();
    }

    /// `Ctrl+u`.
    pub fn kill_bol(&mut self) {
        self.text.replace_range(0..self.caret, "");
        self.caret = 0;
    }

    /// `Ctrl+k`.
    pub fn kill_eol(&mut self) {
        self.text.truncate(self.caret);
    }

    /// `Ctrl+w`: the whitespace before the caret and then the word before that.
    /// Killing the whitespace first is what makes a second `Ctrl+w` eat a whole
    /// word rather than the gap it just made.
    pub fn kill_word_back(&mut self) {
        let start = word_start(&self.text, self.caret);
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
    }

    fn prev_boundary(&self) -> Option<usize> {
        if self.caret == 0 {
            return None;
        }
        (0..self.caret)
            .rev()
            .find(|i| self.text.is_char_boundary(*i))
    }

    fn next_boundary(&self) -> Option<usize> {
        if self.caret >= self.text.len() {
            return None;
        }
        (self.caret + 1..=self.text.len()).find(|i| self.text.is_char_boundary(*i))
    }
}

/// Where `Ctrl+w` should cut back to from `caret`.
fn word_start(text: &str, caret: usize) -> usize {
    let head = &text[..caret];
    let trimmed = head.trim_end();
    // Everything before the caret is blank: take it all, rather than leaving a
    // line of spaces that looks empty and is not.
    if trimmed.is_empty() {
        return 0;
    }
    match trimmed.rfind(char::is_whitespace) {
        // `rfind` gives the byte the separator starts at; the word begins after
        // it, which for a multi-byte separator is not that byte plus one.
        Some(at) => at + trimmed[at..].chars().next().map(char::len_utf8).unwrap_or(1),
        None => 0,
    }
}

/// What the bar is being typed into, which is also what its title says.
///
/// Titles are yazi's, verbatim (PLAN §3: the defaults *are* the yazi config) —
/// they are the words the muscle memory expects to see at the bottom of the
/// window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// `f` — narrows the listing as you type.
    Filter,
    /// `/` — moves the cursor to the next match as you type.
    FindNext,
    /// `?` — the same, backwards.
    FindPrev,
    /// `f` inside the help browser.
    HelpFilter,
}

impl PromptKind {
    pub fn title(self) -> &'static str {
        match self {
            PromptKind::Filter => "Filter:",
            PromptKind::FindNext => "Find next:",
            PromptKind::FindPrev => "Find previous:",
            PromptKind::HelpFilter => "Filter help:",
        }
    }

    /// Whether this prompt drives the help overlay rather than the listing.
    pub fn is_help(self) -> bool {
        matches!(self, PromptKind::HelpFilter)
    }
}

/// One open input bar.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub kind: PromptKind,
    pub line: Line,
    /// Where the cursor was when the prompt opened.
    ///
    /// `/` and `?` search *live*, and every keystroke re-searches from here
    /// rather than from wherever the last keystroke left the cursor — otherwise
    /// deleting a character would leave the cursor on a match for a query that
    /// no longer exists, and the search would walk forward through the
    /// directory as you typed. It is also where `Esc` puts the cursor back.
    pub origin: usize,
}

impl Prompt {
    pub fn new(kind: PromptKind, origin: usize) -> Prompt {
        Prompt {
            kind,
            line: Line::new(),
            origin,
        }
    }

    pub fn query(&self) -> &str {
        self.line.text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> Line {
        let mut l = Line::new();
        l.insert(text);
        l
    }

    #[test]
    fn typing_lands_at_the_caret() {
        let mut l = line("abc");
        assert_eq!((l.text(), l.caret()), ("abc", 3));
        l.move_left();
        l.insert("X");
        assert_eq!((l.text(), l.caret()), ("abXc", 3));
    }

    /// The caret must step over a whole character, not a byte of one — the
    /// difference between working and panicking on any name with an accent.
    #[test]
    fn the_caret_moves_by_characters_not_bytes() {
        let mut l = line("aé☃");
        assert_eq!(l.caret(), l.text().len());
        l.move_left();
        assert_eq!(l.caret(), 3); // before the snowman (3 bytes)
        l.move_left();
        assert_eq!(l.caret(), 1); // before the é (2 bytes)
        l.move_left();
        assert_eq!(l.caret(), 0);
        l.move_left();
        assert_eq!(l.caret(), 0, "at the start it stays");
        l.move_right();
        assert_eq!(l.caret(), 1);
    }

    #[test]
    fn backspace_and_delete_take_one_character_each() {
        let mut l = line("aé☃");
        l.backspace();
        assert_eq!(l.text(), "aé");
        l.move_bol();
        l.delete_under();
        assert_eq!((l.text(), l.caret()), ("é", 0));
        l.backspace();
        assert_eq!(l.text(), "é", "backspace at the start does nothing");
        l.move_eol();
        l.delete_under();
        assert_eq!(l.text(), "é", "delete at the end does nothing");
    }

    #[test]
    fn the_kill_keys_cut_from_the_caret() {
        let mut l = line("one two");
        l.move_left();
        l.kill_eol();
        assert_eq!(l.text(), "one tw");
        l.kill_bol();
        assert_eq!((l.text(), l.caret()), ("", 0));
    }

    /// `Ctrl+w` eats the gap and then the word, so pressing it twice removes
    /// two words rather than a word and a space.
    #[test]
    fn kill_word_back_takes_the_gap_with_the_word() {
        let mut l = line("one two three   ");
        l.kill_word_back();
        assert_eq!(l.text(), "one two ");
        l.kill_word_back();
        assert_eq!(l.text(), "one ");
        l.kill_word_back();
        assert_eq!(l.text(), "");
        l.kill_word_back();
        assert_eq!(l.text(), "", "on an empty line it does nothing");
    }

    /// A separator that is not one byte wide must not leave half of itself
    /// behind.
    #[test]
    fn kill_word_back_handles_a_wide_separator() {
        // U+3000 IDEOGRAPHIC SPACE is whitespace and three bytes long.
        let mut l = line("one\u{3000}two");
        l.kill_word_back();
        assert_eq!(l.text(), "one\u{3000}");
    }

    #[test]
    fn every_prompt_has_a_title() {
        for kind in [
            PromptKind::Filter,
            PromptKind::FindNext,
            PromptKind::FindPrev,
            PromptKind::HelpFilter,
        ] {
            assert!(kind.title().ends_with(':'), "{kind:?}");
        }
        assert!(PromptKind::HelpFilter.is_help());
        assert!(!PromptKind::Filter.is_help());
    }
}
