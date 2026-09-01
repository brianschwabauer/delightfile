//! The prompt behind the top row — and now behind the rename popup, the
//! create prompt and the shell line as well.
//!
//! PLAN §4.2 asks for **one** input implementation shared by rename, filter,
//! create, cd, search and shell, with the full vi line editor on it. That editor
//! is [`df_core::input::InputBuffer`], and this file is the thin app-side shell
//! around it: which prompt is open, what its title says, where it is drawn, and
//! the inline error it shows when what was typed cannot be used.
//!
//! Phase 1 had a byte-offset `Line` here with backspace and the kill keys on it.
//! It is gone rather than kept alongside: two line editors in one program is
//! two sets of Unicode edge cases, and the one in df-core is the one with the
//! modes, the operators and the tests.

use df_core::input::{InputBuffer, InputEvent};
use df_core::keymap::{Chord, InputMode};

/// What the prompt is being typed into, which is also what its title says.
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
    /// `a` — a name, with a trailing `/` for a directory.
    Create,
    /// `r` — the whole name, caret before the extension.
    Rename,
    /// `R` — the extension only, caret at the start.
    RenameEmptyStem,
    /// `;` — a shell line, detached.
    Shell,
    /// `:` — a shell line delightfile waits for.
    ShellBlock,
    /// The conflict dialog's "keep both, under this name".
    ConflictRename,
}

impl PromptKind {
    pub fn title(self) -> &'static str {
        match self {
            PromptKind::Filter => "Filter:",
            PromptKind::FindNext => "Find next:",
            PromptKind::FindPrev => "Find previous:",
            PromptKind::HelpFilter => "Filter help:",
            PromptKind::Create => "Create:",
            PromptKind::Rename | PromptKind::RenameEmptyStem => "Rename:",
            PromptKind::Shell => "Shell:",
            PromptKind::ShellBlock => "Shell (block):",
            PromptKind::ConflictRename => "New name:",
        }
    }

    /// Whether this prompt drives the help overlay rather than the listing.
    pub fn is_help(self) -> bool {
        matches!(self, PromptKind::HelpFilter)
    }

    /// Whether every keystroke changes something behind the prompt. The live
    /// ones re-run their query as you type; the rest do nothing until `Enter`,
    /// which is what makes a half-typed `rm` command harmless.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            PromptKind::Filter
                | PromptKind::FindNext
                | PromptKind::FindPrev
                | PromptKind::HelpFilter
        )
    }

    /// Whether the prompt floats over the row it is about instead of sitting in
    /// the bar — yazi's rename geometry (PLAN §4.2), where the name you are
    /// editing is under your eyes and not at the other end of the window.
    pub fn anchored(self) -> bool {
        matches!(
            self,
            PromptKind::Rename | PromptKind::RenameEmptyStem | PromptKind::ConflictRename
        )
    }
}

/// One open prompt.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub kind: PromptKind,
    pub buffer: InputBuffer,
    /// Where the cursor was when the prompt opened.
    ///
    /// `/` and `?` search *live*, and every keystroke re-searches from here
    /// rather than from wherever the last keystroke left the cursor — otherwise
    /// deleting a character would leave the cursor on a match for a query that
    /// no longer exists, and the search would walk forward through the
    /// directory as you typed. It is also where `Esc` puts the cursor back.
    pub origin: usize,
    /// Why the last `Enter` was refused: "exists", "not a usable name". Drawn
    /// beside the field rather than raised as a toast, because it is about the
    /// text under the caret and belongs where that text is (PLAN §5).
    pub error: Option<String>,
}

impl Prompt {
    /// A prompt over a buffer somebody else prepared — the rename presets, or a
    /// filter re-opened on the query that is already applied.
    pub fn with(kind: PromptKind, origin: usize, buffer: InputBuffer) -> Prompt {
        Prompt {
            kind,
            buffer,
            origin,
            error: None,
        }
    }

    pub fn query(&self) -> &str {
        self.buffer.text()
    }

    /// The caret as a byte offset, which is what the painter measures with.
    pub fn caret(&self) -> usize {
        self.buffer.cursor_byte()
    }

    /// One keystroke. Typing clears a stale error: the message was about the
    /// text as it stood, and it does not stand any more.
    pub fn feed(&mut self, chord: Chord) -> InputEvent {
        let event = self.buffer.feed(chord);
        if matches!(event, InputEvent::Consumed) {
            self.error = None;
        }
        event
    }

    /// The mode chip's text. Shown for every prompt, because the whole point of
    /// a modal editor is that you can tell which mode you are in.
    pub fn mode_label(&self) -> &'static str {
        match self.buffer.mode() {
            InputMode::Insert => "INSERT",
            InputMode::Normal => "NORMAL",
            InputMode::Visual => "VISUAL",
            InputMode::Replace => "REPLACE",
        }
    }

    /// Whether the caret is a block (Normal, Visual, Replace — it sits *on* a
    /// character) or a bar (Insert — it sits *between* two).
    pub fn block_caret(&self) -> bool {
        !matches!(self.buffer.mode(), InputMode::Insert)
    }

    /// The visual selection, in bytes.
    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        self.buffer.selection_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use df_core::keymap::{Key, Mods};

    fn chord(c: char) -> Chord {
        Chord::from_char(c).expect("a printable key")
    }

    #[test]
    fn every_prompt_has_a_title() {
        for kind in [
            PromptKind::Filter,
            PromptKind::FindNext,
            PromptKind::FindPrev,
            PromptKind::HelpFilter,
            PromptKind::Create,
            PromptKind::Rename,
            PromptKind::RenameEmptyStem,
            PromptKind::Shell,
            PromptKind::ShellBlock,
            PromptKind::ConflictRename,
        ] {
            assert!(kind.title().ends_with(':'), "{kind:?}");
        }
        assert!(PromptKind::HelpFilter.is_help());
        assert!(!PromptKind::Filter.is_help());
        assert!(PromptKind::Filter.is_live());
        assert!(
            !PromptKind::Shell.is_live(),
            "a shell line runs on Enter only"
        );
        assert!(PromptKind::Rename.anchored());
        assert!(!PromptKind::Create.anchored());
    }

    /// The prompt is the df-core editor: modes, motions and all, with the app
    /// only holding the frame around it.
    ///
    /// In `[input] vi_mode`, because that is what puts `Esc` on the ladder
    /// rather than on "close this" — the shipped default is the next test.
    #[test]
    fn the_prompt_is_the_vi_editor() {
        let mut prompt = Prompt::with(
            PromptKind::Rename,
            0,
            InputBuffer::for_rename_stem("photo.jpg").vi_mode(true),
        );
        assert_eq!(prompt.query(), "photo.jpg");
        assert_eq!(
            prompt.caret(),
            "photo".len(),
            "the caret is before the extension"
        );
        assert_eq!(prompt.mode_label(), "INSERT");
        assert!(!prompt.block_caret());

        // Escape to Normal, `0` to the start, `D` to kill the line's tail.
        assert_eq!(prompt.feed(Chord::plain(Key::Escape)), InputEvent::Consumed);
        assert_eq!(prompt.mode_label(), "NORMAL");
        assert!(
            prompt.block_caret(),
            "a normal-mode caret sits on a character"
        );
        prompt.feed(chord('0'));
        prompt.feed(Chord::new(Mods::SHIFT, Key::Char('d')));
        assert_eq!(prompt.query(), "");

        // …and typing goes back through insert.
        prompt.feed(chord('i'));
        for c in "cat.png".chars() {
            prompt.feed(chord(c));
        }
        assert_eq!(prompt.query(), "cat.png");
        assert_eq!(
            prompt.feed(Chord::plain(Key::Enter)),
            InputEvent::Submit("cat.png".to_string())
        );
    }

    /// …and by default `Esc` closes the prompt on the first press, with no
    /// block caret in between (PLAN §4.2, `[input] vi_mode = false`).
    #[test]
    fn escape_cancels_the_prompt_by_default() {
        let mut prompt = Prompt::with(
            PromptKind::Rename,
            0,
            InputBuffer::for_rename_stem("photo.jpg"),
        );
        assert_eq!(prompt.feed(Chord::plain(Key::Escape)), InputEvent::Cancel);
        assert_eq!(prompt.mode_label(), "INSERT");
        assert!(!prompt.block_caret());
    }

    /// `R` opens on the extension alone, caret at the front (PLAN §4.1).
    #[test]
    fn rename_with_an_empty_stem_keeps_the_extension() {
        let prompt = Prompt::with(
            PromptKind::RenameEmptyStem,
            0,
            InputBuffer::for_rename_empty("photo.jpg"),
        );
        assert_eq!(prompt.query(), ".jpg");
        assert_eq!(prompt.caret(), 0);
    }

    /// An error is about the text as it stands, so editing the text takes it
    /// down — but a keystroke that submits or cancels leaves it alone.
    #[test]
    fn typing_clears_the_inline_error() {
        let mut prompt = Prompt::with(PromptKind::Create, 0, InputBuffer::new("", 0));
        prompt.error = Some("notes.txt already exists".to_string());
        prompt.feed(chord('x'));
        assert!(prompt.error.is_none());

        prompt.error = Some("still true".to_string());
        assert!(matches!(
            prompt.feed(Chord::plain(Key::Enter)),
            InputEvent::Submit(_)
        ));
        assert!(prompt.error.is_some(), "submitting does not clear it");
    }
}
