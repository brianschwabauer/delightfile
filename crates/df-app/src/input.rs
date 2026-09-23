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

use df_core::input::{is_word_char, segment_at, InputAction, InputBuffer, InputEvent};
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
    /// How far the field's text is scrolled to the left, in points, as it
    /// was last drawn: what [`crate::chrome::caret_scroll`] starts from, so the
    /// text holds still while the caret moves inside the field and scrolls
    /// only when the caret would leave it. A drag past the field's edge moves
    /// it too. View state rather than editing state, kept here because it
    /// belongs to this prompt and goes when the prompt does.
    pub scroll: f32,
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
            scroll: 0.0,
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

    /// A press in the field (PLAN §7.5).
    ///
    /// `at` is the character boundary nearest the pointer and `under` the
    /// character it is on — the caller measured both against the layout the
    /// field is drawn from ([`crate::chrome::FieldGeom`]). `clicks` is how far
    /// the run of quick presses has got ([`crate::mouse::Clicks::count`]):
    ///
    /// * one puts the caret at `at`, or with `shift` held grows the selection
    ///   from its anchor — the caret, if nothing was selected — to `at`, as in
    ///   every text field;
    /// * two selects the segment around `under` ([`Prompt::segment_at`]);
    /// * three selects the whole line.
    ///
    /// A stale error is left standing. It is about the text, and a click
    /// changes where the caret is, not what the text says.
    pub fn click(&mut self, at: usize, under: usize, clicks: u8, shift: bool) {
        match clicks {
            2 => {
                let run = self.segment_at(under);
                self.buffer.set_selection(run.start, run.end);
            }
            3.. => {
                let len = self.buffer.text().chars().count();
                self.buffer.set_selection(0, len);
            }
            _ => self.buffer.move_to(at, shift),
        }
    }

    /// A drag from a single press in the field has reached boundary `at`: the
    /// selection runs from where the press left the anchor — the press point,
    /// or the old anchor for a Shift+press — to here.
    pub fn drag_to(&mut self, at: usize) {
        self.buffer.move_to(at, true);
    }

    /// The run a double click on character `at` selects.
    ///
    /// In `Go to:` the run is a path segment, between `/`s: the thing a person
    /// double-clicks in a path is a directory name, and the word motions would
    /// stop inside `delight-file` or `v1.2`. Everywhere else it is a word, by
    /// the classes `Ctrl+←/→` step over, so the pointer and the keys agree on
    /// what a word is.
    pub fn segment_at(&self, at: usize) -> std::ops::Range<usize> {
        let text = self.buffer.text();
        match self.kind {
            PromptKind::Path => segment_at(text, at, |c| c == '/'),
            _ => segment_at(text, at, |c| !is_word_char(c)),
        }
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

    /// The pointer in `Go to:`: a click is a caret, a drag a selection from the
    /// press point, two clicks a path segment, three the line — and Shift grows
    /// what is there.
    #[test]
    fn the_pointer_selects_a_path_by_segment() {
        let path = "/home/brian/Downloads";
        let mut prompt = Prompt::with(PromptKind::Path, 0, InputBuffer::new(path, 21));

        prompt.click(7, 7, 1, false);
        assert_eq!(prompt.buffer.cursor(), 7);
        assert_eq!(prompt.buffer.selection(), None);
        prompt.drag_to(15);
        assert_eq!(prompt.buffer.selection(), Some(7..15));
        prompt.drag_to(3);
        assert_eq!(prompt.buffer.selection(), Some(3..7), "back past the press");

        // Two clicks: the segment the pointer is on, caret at its end.
        prompt.click(8, 7, 2, false);
        assert_eq!(prompt.buffer.selection(), Some(6..11), "brian");
        assert_eq!(prompt.buffer.cursor(), 11);
        // …on the `/` itself: the segment it introduces.
        prompt.click(11, 11, 2, false);
        assert_eq!(prompt.buffer.selection(), Some(12..21), "Downloads");

        // Three: everything.
        prompt.click(8, 7, 3, false);
        assert_eq!(prompt.buffer.selection(), Some(0..21));

        // Shift+click grows the selection from the caret it had.
        prompt.click(6, 6, 1, false);
        prompt.click(11, 10, 1, true);
        assert_eq!(prompt.buffer.selection(), Some(6..11));
        prompt.click(1, 1, 1, true);
        assert_eq!(
            prompt.buffer.selection(),
            Some(1..6),
            "from the same anchor"
        );
    }

    /// Every other prompt double-clicks by word, the way `Ctrl+←/→` reads one.
    #[test]
    fn the_pointer_selects_a_query_by_word() {
        let mut prompt = Prompt::with(
            PromptKind::Shell,
            0,
            InputBuffer::new("cp notes.txt /tmp/old-notes", 0),
        );
        prompt.click(5, 4, 2, false);
        assert_eq!(
            prompt.buffer.selection(),
            Some(3..8),
            "notes, not notes.txt"
        );
        prompt.click(20, 20, 2, false);
        assert_eq!(prompt.buffer.selection(), Some(18..21), "old");

        // A click is not an edit: the error it lands beside still stands.
        prompt.error = Some("no such command".to_string());
        prompt.click(0, 0, 1, false);
        assert!(prompt.error.is_some());
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
