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
    /// `Ctrl+l`, or a click on the breadcrumb's last segment: the directory
    /// you are in, as a whole path you can edit, paste over, and `Enter` to go
    /// to.
    Path,
    /// `c` in the mount manager: a server address for `gio mount`.
    Connect,
    /// A save dialog's Save button: the name to save under, in the directory
    /// on screen.
    SaveAs,
    /// `A`: the name of the archive the selection is packed into. Its
    /// extension is the format.
    Archive,
    /// `g b` on a folder that is not pinned: the key after `g` that will go
    /// there, or nothing for a pin with no key. Its title names the folder
    /// ([`Prompt::label`]), so the bar does not name it a second time.
    Pin,
    /// `T`: the tags of the row under the cursor, or of a selection, as a
    /// comma-separated line. Its title names what is being tagged
    /// ([`Prompt::label`]): `Tags of notes.txt:`, `Tags of 3 items:`.
    Tags,
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
            PromptKind::SaveAs => "Save as:",
            PromptKind::Archive => "Archive as:",
            PromptKind::Pin => "Pin as:",
            PromptKind::Tags => "Tags:",
        }
    }

    /// Whether the bar keeps the directory the prompt is about at its far
    /// left. Every bar prompt does except `Go to:`, whose field *is* that
    /// directory, spelled out in full — the same path twice on one line would
    /// be the second copy crowding out the one you are editing.
    pub fn shows_directory(self) -> bool {
        !matches!(self, PromptKind::Path | PromptKind::Pin)
    }

    /// The quiet word a prompt always says beside its field, when it has
    /// one: what an empty `Enter` does is not something a person can guess
    /// from a blank field, so the prompt that has an answer for it says it.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            PromptKind::Pin => Some("a key after g, or Enter for none"),
            // Neither is guessable from a field of words: that a comma is
            // what separates them, and that `Tab` finishes one.
            PromptKind::Tags => Some("commas between tags · Tab completes"),
            _ => None,
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

/// What a press somewhere else in the window does to an open prompt, before
/// the press itself is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickOutside {
    /// Close it the way `Esc` does: nothing typed is applied.
    Cancel,
    /// Close it the way `Enter` does: what it is already showing stays.
    Commit,
    /// Leave it open. The surface it lives in owns the pointer.
    Keep,
}

/// How a click outside `kind` resolves it.
///
/// The split is [`PromptKind::is_live`]'s. A live prompt has been showing its
/// effect since the first letter, the listing narrowed or the cursor on the
/// match, so clicking away keeps what is on screen: undoing a filter the
/// reader was looking at, because they reached for the mouse to use it, would
/// take away the thing they had just made. Every other prompt does nothing
/// until `Enter`, and a click elsewhere is not an `Enter`: a half-typed shell
/// line or a name for `a` is dropped, never run.
///
/// Two stay open whatever is clicked. The conflict dialog's rename is a field
/// inside a modal card whose scrim already takes every press, and the help
/// filter belongs to the help sheet, which goes (and takes its filter with it)
/// on the sheet's own terms.
pub fn click_outside_action(kind: PromptKind) -> ClickOutside {
    match kind {
        PromptKind::Path
        | PromptKind::Create
        | PromptKind::Rename
        | PromptKind::RenameEmptyStem
        | PromptKind::Shell
        | PromptKind::ShellBlock
        | PromptKind::Connect
        | PromptKind::SaveAs
        | PromptKind::Archive
        | PromptKind::Pin
        | PromptKind::Tags => ClickOutside::Cancel,
        PromptKind::Filter | PromptKind::FindNext | PromptKind::FindPrev => ClickOutside::Commit,
        PromptKind::ConflictRename | PromptKind::HelpFilter => ClickOutside::Keep,
    }
}

/// How one run of an [`InkedHint`] is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// The hint's own quiet colour.
    Quiet,
    /// The part the hint is about: the format the typed name picks.
    Strong,
    /// Something that is not available here: a format whose program is not
    /// installed.
    Absent,
    /// The part the hint is warning about: the picked format cannot be
    /// written, and why.
    Warn,
}

/// A hint made of runs in different inks. One string, so it is measured,
/// truncated and laid out exactly as a plain hint is; the runs only say which
/// bytes of it take which colour.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InkedHint {
    text: String,
    runs: Vec<(std::ops::Range<usize>, Ink)>,
}

impl InkedHint {
    pub fn push(&mut self, text: &str, ink: Ink) {
        let start = self.text.len();
        self.text.push_str(text);
        self.runs.push((start..self.text.len(), ink));
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Byte ranges of [`InkedHint::text`], in order, covering all of it.
    pub fn runs(&self) -> &[(std::ops::Range<usize>, Ink)] {
        &self.runs
    }

    /// The text of the runs in `ink`, for a test that wants to know what is
    /// lit without measuring anything.
    #[cfg(test)]
    pub fn inked(&self, ink: Ink) -> Vec<&str> {
        self.runs
            .iter()
            .filter(|(_, i)| *i == ink)
            .map(|(range, _)| &self.text[range.clone()])
            .collect()
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
    /// A quiet word about what `Enter` will do, drawn where the error goes and
    /// in a colour that is not the error's: the filter's "No matches here ·
    /// Enter searches everywhere". Not a complaint — nothing typed is wrong —
    /// so an error, when there is one, is said instead.
    ///
    /// Owned by the app rather than by the editing here, which is why typing
    /// does not clear it: it is about the listing behind the prompt, and the
    /// app sets it afresh every frame from what that listing shows.
    pub hint: Option<&'static str>,
    /// A hint in more than one ink, said where [`Prompt::hint`] would be when
    /// that has nothing to say: the archive prompt's list of formats, the one
    /// the name picks lit and the ones this machine cannot write dimmed. Set
    /// every frame by the app, like the plain hint.
    pub inked: Option<InkedHint>,
    /// How far the field's text is scrolled to the left, in points, as it
    /// was last drawn: what [`crate::chrome::caret_scroll`] starts from, so the
    /// text holds still while the caret moves inside the field and scrolls
    /// only when the caret would leave it. A drag past the field's edge moves
    /// it too. View state rather than editing state, kept here because it
    /// belongs to this prompt and goes when the prompt does.
    pub scroll: f32,
    /// A title of this prompt's own, in place of its kind's: `Pin ~/Work
    /// as:`, where the folder being pinned is what the question is about.
    pub label: Option<String>,
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
            hint: None,
            inked: None,
            scroll: 0.0,
            label: None,
        }
    }

    /// What the title says: its own label, or its kind's.
    pub fn title(&self) -> &str {
        self.label.as_deref().unwrap_or(self.kind.title())
    }

    pub fn query(&self) -> &str {
        self.buffer.text()
    }

    /// What the line says beside the query, and whether it is an error: the
    /// error when there is one, the hint when there is not — the app's for
    /// this frame, or the one the kind always says ([`PromptKind::hint`]).
    pub fn message(&self) -> Option<(&str, bool)> {
        match (&self.error, self.hint.or(self.kind.hint())) {
            (Some(error), _) => Some((error.as_str(), true)),
            (None, Some(hint)) => Some((hint, false)),
            (None, None) => self.inked.as_ref().map(|inked| (inked.text(), false)),
        }
    }

    /// The inked hint, when it is what [`Prompt::message`] is saying — so the
    /// painter can colour the words it has already measured.
    pub fn inked_message(&self) -> Option<&InkedHint> {
        if self.error.is_some() || self.hint.is_some() {
            return None;
        }
        self.inked.as_ref()
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

    /// What `input-copy` hands the clipboard: the selection, or the whole
    /// line when nothing is selected; `None` for an empty field
    /// (05-defaults-and-config.md §3).
    pub fn copy_text(&self) -> Option<&str> {
        let text = self.buffer.text();
        let copied = match self.buffer.selection_bytes() {
            Some(range) => text.get(range).unwrap_or(text),
            None => text,
        };
        (!copied.is_empty()).then_some(copied)
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

    /// `input-copy` copies the selection, the whole line when nothing is
    /// selected, and nothing from an empty field.
    #[test]
    fn a_copy_takes_the_selection_or_else_the_line() {
        let mut prompt = Prompt::with(PromptKind::Rename, 0, InputBuffer::new("holiday photos", 0));
        assert_eq!(prompt.copy_text(), Some("holiday photos"));
        prompt.buffer.set_selection(8, 14);
        assert_eq!(prompt.copy_text(), Some("photos"));
        let empty = Prompt::with(PromptKind::Filter, 0, InputBuffer::new("", 0));
        assert_eq!(empty.copy_text(), None);
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
            PromptKind::SaveAs,
            PromptKind::Archive,
            PromptKind::Pin,
            PromptKind::Tags,
        ] {
            assert!(kind.title().ends_with(':'), "{kind:?}");
        }
        assert!(
            !PromptKind::Tags.is_live() && !PromptKind::Tags.anchored(),
            "tags are written on Enter only, and typed in the bar"
        );
        assert!(
            !PromptKind::Archive.is_live() && !PromptKind::Archive.anchored(),
            "an archive name packs nothing until Enter, and it is typed in the bar"
        );
        assert!(
            !PromptKind::SaveAs.is_live() && !PromptKind::SaveAs.anchored(),
            "a save name picks nothing until Enter, and it is typed in the bar"
        );
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

    /// The pin prompt names the folder in its own title, says what an empty
    /// `Enter` does whatever the app's per-frame hint is, and gives way to an
    /// error like any other hint.
    #[test]
    fn the_pin_prompt_says_what_it_pins_and_what_enter_does() {
        let mut prompt = Prompt::with(PromptKind::Pin, 0, InputBuffer::new("", 0));
        assert_eq!(prompt.title(), "Pin as:");
        prompt.label = Some("Pin ~/Work as:".to_string());
        assert_eq!(prompt.title(), "Pin ~/Work as:");
        assert!(!PromptKind::Pin.shows_directory(), "the title names it");
        assert_eq!(
            prompt.message(),
            Some(("a key after g, or Enter for none", false))
        );
        prompt.hint = None;
        assert!(
            prompt.message().is_some(),
            "the app's frame does not clear it"
        );
        prompt.error = Some("bad".to_string());
        assert_eq!(prompt.message(), Some(("bad", true)));
        assert_eq!(PromptKind::Filter.hint(), None);
    }

    /// A click away keeps what a live prompt is already showing and drops what
    /// a quiet one has not done yet. The two that live inside another surface
    /// are left to it.
    #[test]
    fn a_click_outside_keeps_only_what_is_already_on_screen() {
        use ClickOutside::{Cancel, Commit, Keep};
        for (kind, expected) in [
            (PromptKind::Filter, Commit),
            (PromptKind::FindNext, Commit),
            (PromptKind::FindPrev, Commit),
            (PromptKind::HelpFilter, Keep),
            (PromptKind::Create, Cancel),
            (PromptKind::Rename, Cancel),
            (PromptKind::RenameEmptyStem, Cancel),
            (PromptKind::Shell, Cancel),
            (PromptKind::ShellBlock, Cancel),
            (PromptKind::ConflictRename, Keep),
            (PromptKind::Path, Cancel),
            (PromptKind::Connect, Cancel),
            (PromptKind::SaveAs, Cancel),
            (PromptKind::Archive, Cancel),
            (PromptKind::Pin, Cancel),
            (PromptKind::Tags, Cancel),
        ] {
            assert_eq!(click_outside_action(kind), expected, "{kind:?}");
            // The rule the table is written from: a prompt that waits for
            // `Enter` is never run by a click that was aimed somewhere else.
            if !kind.is_live() {
                assert_ne!(expected, Commit, "{kind:?} would act on a click");
            }
        }
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

    /// An inked hint is one string with colours on it, said only when there
    /// is no error and no plain hint to say instead.
    #[test]
    fn an_inked_hint_is_said_when_nothing_else_is() {
        let mut prompt = Prompt::with(PromptKind::Archive, 0, InputBuffer::new("a.zip", 1));
        let mut inked = InkedHint::default();
        inked.push("zip", Ink::Strong);
        inked.push(" · ", Ink::Quiet);
        inked.push("7z", Ink::Absent);
        // Byte ranges: the middle dot is two bytes.
        assert_eq!(inked.runs()[2].0, 7..9);
        assert_eq!(inked.inked(Ink::Strong), ["zip"]);
        prompt.inked = Some(inked);
        assert_eq!(prompt.message(), Some(("zip · 7z", false)));
        assert!(prompt.inked_message().is_some());

        prompt.error = Some("bad".to_string());
        assert_eq!(prompt.message(), Some(("bad", true)));
        assert!(
            prompt.inked_message().is_none(),
            "the error is what is said"
        );
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
