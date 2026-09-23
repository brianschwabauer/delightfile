//! The prompt behind the top row — and now behind the rename popup, the
//! create prompt and the shell line as well.
//!
//! PLAN §4.2 asks for **one** input implementation shared by rename, filter,
//! create, cd, search and shell. That editor
//! is [`df_core::input::InputBuffer`], and this file is the thin app-side shell
//! around it: which prompt is open, what its title says, where it is drawn, and
//! the inline error it shows when what was typed cannot be used.
//!
//! Phase 1 had a byte-offset `Line` here with backspace and the kill keys on it.
//! It is gone rather than kept alongside: two line editors in one program is
//! two sets of Unicode edge cases, and the one in df-core is the one with the
//! word motions, the selection and the tests.

use df_core::input::{InputAction, InputBuffer, InputEvent};
use df_core::keymap::Chord;

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
    /// A click on the breadcrumb's last segment: the directory you are in,
    /// as a whole path you can edit, paste over, and `Enter` to go to.
    Path,
    /// `c` in the mount manager: a server address for `gio mount`.
    Connect,
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
            PromptKind::Path => "Go to:",
            PromptKind::Connect => "Connect to:",
        }
    }

    /// Whether the bar keeps the directory the prompt is about at its far
    /// left. Every bar prompt does except `Go to:`, whose field *is* that
    /// directory, spelled out in full — the same path twice on one line would
    /// be the second copy crowding out the one you are editing.
    pub fn shows_directory(self) -> bool {
        !matches!(self, PromptKind::Path)
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

    /// One resolved action — what the keymap's `[input]` table asked for. The
    /// same error rule as [`Prompt::feed`]: an edit takes a stale message down,
    /// a submit or a cancel leaves it.
    pub fn act(&mut self, action: InputAction) -> InputEvent {
        let event = self.buffer.act(action);
        if matches!(event, InputEvent::Consumed) {
            self.error = None;
        }
        event
    }

    /// Already-composed text at the caret: an IME commit, or the clipboard's
    /// answer to `Ctrl+v`. See [`InputBuffer::insert_text`].
    pub fn insert_text(&mut self, text: &str) -> InputEvent {
        let event = self.buffer.insert_text(text);
        if matches!(event, InputEvent::Consumed) {
            self.error = None;
        }
        event
    }

    /// The selection, in bytes.
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
            PromptKind::Path,
            PromptKind::Connect,
        ] {
            assert!(kind.title().ends_with(':'), "{kind:?}");
        }
        assert!(
            !PromptKind::Path.shows_directory(),
            "the field already holds the whole path"
        );
        assert!(PromptKind::Filter.shows_directory());
        assert!(
            !PromptKind::Path.is_live() && !PromptKind::Path.anchored(),
            "a typed path goes nowhere until Enter, and it is typed in the bar"
        );
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

    /// The prompt is the df-core editor: motions, kills and all, with the app
    /// only holding the frame around it — and no mode to be in, so every
    /// letter types itself from the first keystroke.
    #[test]
    fn the_prompt_is_the_line_editor() {
        let mut prompt = Prompt::with(
            PromptKind::Rename,
            0,
            InputBuffer::for_rename_stem("photo.jpg"),
        );
        assert_eq!(prompt.query(), "photo.jpg");
        assert_eq!(
            prompt.caret(),
            "photo".len(),
            "the caret is before the extension"
        );

        // `i` is an `i`, not a mode; `Ctrl+u` kills back to the start.
        prompt.feed(chord('i'));
        assert_eq!(prompt.query(), "photoi.jpg");
        prompt.feed(Chord::new(Mods::CTRL, Key::Char('u')));
        assert_eq!(prompt.query(), ".jpg");

        prompt.feed(Chord::new(Mods::CTRL, Key::Char('k')));
        assert_eq!(prompt.query(), "");
        for c in "cat.png".chars() {
            prompt.feed(chord(c));
        }
        assert_eq!(
            prompt.feed(Chord::plain(Key::Enter)),
            InputEvent::Submit("cat.png".to_string())
        );
    }

    /// `Esc` closes the prompt on the first press, always (PLAN §4.2).
    #[test]
    fn escape_cancels_the_prompt() {
        let mut prompt = Prompt::with(
            PromptKind::Rename,
            0,
            InputBuffer::for_rename_stem("photo.jpg"),
        );
        assert_eq!(prompt.feed(Chord::plain(Key::Escape)), InputEvent::Cancel);
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
