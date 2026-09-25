//! The bulk-rename card's right column: every new name, as one document
//! (PLAN §5).
//!
//! The card lists the files being renamed, the old name on the left and the
//! new one on the right. The right column could have been one text field per
//! file, and that version makes the common jobs tedious. Adding `-final` to
//! forty names would be forty clicks, forty `End`s and forty pastes. So it is
//! one editor over forty lines, and it can hold more than one caret.
//! `Ctrl+Shift+↓` stacks a caret on every row, `End` sends each one to the end
//! of its own line, and a word typed once lands forty times. `Ctrl+d` selects
//! the word under the caret and then each further place it occurs, so changing
//! `IMG` to `holiday` in every name is one chord per name and one word. These
//! are Zed's gestures on Zed's keys, because a person who knows them from an
//! editor should not have to learn them again for a file manager.
//!
//! # A row is a file
//!
//! This is not a text editor, because line *n* is file *n*'s new name and
//! nothing may break that pairing. There is no newline. `Enter` submits the
//! card instead of splitting a row, since a split would make a name with no
//! file behind it. `Backspace` at the start of a row and `Delete` at its end do
//! nothing, since a join would give two files one name and leave a row empty.
//! A paste has its newlines dropped. A selection never spans rows either,
//! because replacing one that did would be a join by another route. So a
//! caret's anchor and head are always on the same row, and a selection is a
//! char range on that row.
//!
//! The one way a row changes place is whole. `Alt+↑` and `Alt+↓` swap it with
//! its neighbour, and [`NamesEditor::take_swap`] reports the swap. The app
//! keeps the old names and each file's facts in vectors parallel to these
//! lines, and those have to swap too or the card would rename the wrong file.
//!
//! # Carets
//!
//! A caret has an anchor and a head, which are equal when nothing is selected,
//! plus a goal column and an age.
//!
//! The goal column is where `↑` and `↓` aim. A stack of carets at column 12
//! that crosses a six-character name lands at 6 on that row and back at 12 on
//! the row after it. Without the goal, one short name would drag the whole
//! stack left for the rest of the list, and `Ctrl+Shift+↓` down a column of
//! names of different lengths would be useless.
//!
//! The age orders carets by when they were added. The youngest is the
//! primary: `Ctrl+d` searches onward from it, `Alt+↑` moves its row, and `Esc`
//! keeps it when it drops the rest.
//!
//! Carets stay sorted and never double up. Two that land on the same spot
//! become one, which is what happens after `Home` at two carets on one row or
//! `↑` at a stack that reaches the first row. A caret on a selection's edge or
//! inside it joins the selection, and two overlapping selections become their
//! union. Two selections that only touch stay two, because typing over them is
//! still two separate replacements. A merge keeps the younger caret's goal, age
//! and direction, so the primary survives a merge it took part in.
//!
//! Every edit goes through one splice. It takes a list of (row, char range,
//! replacement) triples, applies each row's list in one pass, and then maps
//! every caret on that row through the result. A caret that made an edit lands
//! after the text it inserted. Any other caret keeps its place in the text
//! around it. Before an edit it does not move, after one it shifts by the
//! change in length, and at the end of a replaced range or inside it it lands
//! after the replacement. Typing at forty carets, `Ctrl+w` at forty carets and
//! the template popover's completion ([`NamesEditor::replace_ranges`]) are all
//! that one rule.
//!
//! # Keys
//!
//! Everything [`crate::input`]'s prompt answers to works here, at every caret,
//! on the same readline keys, with one exception. `Ctrl+d` is Zed's "select
//! the next match" rather than readline's "delete under the caret". On a card
//! whose whole point is changing many names at once it is the more useful of
//! the two, and `Delete` still deletes. On top of the prompt's keys there are
//! the row motions (`↑` `↓` `PgUp` `PgDn`, `Ctrl+Home` and `Ctrl+End`), the
//! caret makers (`Ctrl+Shift+↑` and `↓`, `Ctrl+d`, `Ctrl+Shift+l`), `Alt+↑`
//! and `Alt+↓` to move a row, `Enter` to submit and `Esc`.
//!
//! `Esc` works in two stages. With several carets or a live selection it
//! collapses to a single caret where the primary's head is, and only a press
//! with nothing left to collapse closes the card. Otherwise the gesture that
//! exists to get rid of forty carets would also throw away the edit they made.
//!
//! `Ctrl+c` is not `Esc` here, although it is in the prompt. The prompt's
//! `Ctrl+c` is the terminal's "stop", and a one-line field has nothing worth
//! copying out of it. This column has selections at forty carets, and a person
//! who selects text and presses `Ctrl+c` means copy, as in every editor they
//! know. Read as `Esc`, that press would drop the selections or close the card.
//! The clipboard belongs to the app, not to this editor, so both `Ctrl+c` and
//! `Ctrl+x` answer [`EditorEvent::Ignored`]. The card reads
//! [`NamesEditor::selected_text`], and for a cut it also calls
//! [`NamesEditor::delete_selections`].
//!
//! Unlike the prompt, this editor can answer [`EditorEvent::Ignored`]. The
//! prompt swallows every key because a stray one has nowhere safe to go. The
//! card has a template field above the list, a completion popover and focus to
//! move between them, and `Tab`, `Ctrl+↑` and `Ctrl+↓` belong to those. A chord
//! this editor has no verb for goes back to the card instead of vanishing.
//!
//! # Undo
//!
//! An undo step is a full copy of every line and every caret. Names are short,
//! so a copy is cheap and far simpler to get right than a diff. Keeping the
//! carets in the step is what makes `Ctrl+z` after `Ctrl+d` and a typed word
//! put the carets back on the words that were replaced. A run of typing is one
//! step, and any motion or command ends the run. [`NamesEditor::set_lines`]
//! (the template rewrote the column) and [`NamesEditor::replace_ranges`] (a
//! completion) are one step each however many rows they touch.
//! [`NamesEditor::set_lines_quietly`] (a photo's facts arrived) is no step
//! at all, and it leaves a typing run open, because nobody did it. Undoing or
//! redoing a row swap reports the swap again through
//! [`NamesEditor::take_swap`]. A swap is its own inverse, so the app's
//! parallel vectors follow the undo by swapping the same pair.
//!
//! # Touched rows
//!
//! Each row carries a flag for whether a person has changed its text since
//! the template last wrote the column. The card needs it for two things. A
//! photo landing or a row moving rewrites only the rows the template still
//! owns. A value the template could not fill is only a problem on a row that
//! still claims to be the template's.
//!
//! The flag lives here, and in every undo step, because an undo that puts
//! back an older line has to put back whose line it was. The card used to
//! keep the flag itself by remembering the line it last wrote into each row
//! and comparing. That record had no history. Undo could restore a line the
//! card had since rewritten, and the row then looked typed-in to the card
//! even though nobody had typed in it.
//!
//! The rules are few. [`NamesEditor::new`] and [`NamesEditor::set_lines`]
//! leave every row untouched. [`NamesEditor::set_lines_quietly`] leaves the
//! flags as they are, since it only rewrites rows the card already considers
//! untouched. Any edit that changes a row's text touches that row: typing, a
//! paste, a delete or kill, a cut, a completion. A row move carries the flag
//! with the row, and a caret motion never changes it.
//!
//! # Positions and the pointer
//!
//! A column is a char index, as it is in the prompt. [`crate::input`]'s header
//! explains why it is neither a byte offset nor a grapheme. Word motions use the
//! prompt's classes ([`is_word_char`]), and a double click selects the run
//! [`segment_at`] finds, so the word the pointer takes is the word `Ctrl+→`
//! steps over. `Alt+←/→` step by the prompt's smaller word too, which reads
//! `_` as a space: in a column of `2024_holiday_DSC_0198` names it is the key
//! that reaches `holiday` at every caret at once.
//!
//! The pointer arrives as verbs with the geometry already done by whoever
//! painted the rows. [`NamesEditor::click`] places a caret, extends the
//! primary with Shift or adds a caret with Alt. [`NamesEditor::double_click`]
//! selects a word and [`NamesEditor::drag_to`] grows the primary's selection
//! along its row. Nothing here knows what a pixel is, which is what keeps it
//! testable without a display.

use std::ops::Range;

use crate::input::{is_word_char, segment_at};
use crate::keymap::{Chord, Key, Mods};

/// How many undo steps the card keeps.
///
/// Each step is a copy of every name, so the cost is the card's size times
/// this. A thousand 40-byte names is about 64 KB a step, so a full stack is a
/// few megabytes on a large card. A hundred separate edits is already far more
/// than a rename session reaches, because a typed run is one step.
const UNDO_LIMIT: usize = 100;

/// The rows `PgUp` and `PgDn` move before the card has said how many it shows
/// ([`NamesEditor::set_page`]).
const DEFAULT_PAGE: usize = 10;

/// A place between two characters: a row and a char index into it, from 0 to
/// the row's length inclusive.
///
/// Ordered by row first, so a sorted list of positions reads top to bottom and
/// left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub row: usize,
    pub col: usize,
}

/// One caret: where a selection started (`anchor`) and where the caret is
/// (`head`). The two are equal when nothing is selected, and always on the
/// same row (see the module header on why a selection cannot span rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub anchor: Pos,
    pub head: Pos,
}

impl Cursor {
    fn caret(at: Pos) -> Cursor {
        Cursor {
            anchor: at,
            head: at,
        }
    }

    /// Whether this caret has something selected.
    pub fn is_selection(&self) -> bool {
        self.anchor != self.head
    }

    /// The selected chars on the head's row, low end first. Empty, at the
    /// caret, when nothing is selected.
    pub fn range(&self) -> Range<usize> {
        let (a, h) = (self.anchor.col, self.head.col);
        a.min(h)..a.max(h)
    }
}

/// What a keystroke did, from the card's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    /// Handled. Repaint from the accessors.
    Consumed,
    /// `Enter`. The app decides before feeding the key whether a popover
    /// takes it instead, so reaching the editor means submit.
    Submit,
    /// `Esc` (or `Ctrl+[`) with nothing left to collapse. The app closes the
    /// card.
    Cancel,
    /// The editor has no verb for this chord (`Tab`, `Ctrl+↑`, `Ctrl+c`,
    /// anything unbound). The app may use it.
    Ignored,
}

/// One editing verb, named apart from the key that runs it, as
/// [`crate::input::InputOp`] is for the prompt.
///
/// The first block is the prompt's verbs, run at every caret. The rest exist
/// because this editor has rows and more than one caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorOp {
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
    /// The word motions with `_` read as a space, as the prompt's are (see
    /// [`crate::input`]'s header on the two sizes of word).
    SubwordForward,
    SubwordBackward,
    SelectSubwordForward,
    SelectSubwordBackward,
    Backspace,
    DeleteUnder,
    KillBol,
    KillEol,
    KillWordBackward,
    KillWordForward,
    Undo,
    Redo,
    /// Every caret moves a row, aiming for its goal column and stopping at
    /// the end of a shorter line.
    Up,
    Down,
    /// As `Up` and `Down`, by [`NamesEditor::set_page`] rows.
    PageUp,
    PageDown,
    /// One caret, on the first or last row, at the primary's goal column.
    FirstRow,
    LastRow,
    /// A new caret on the row above the topmost caret (below the bottommost),
    /// at that caret's goal column. It becomes the primary.
    AddCursorAbove,
    AddCursorBelow,
    /// With nothing selected at the primary, select the word under it. With a
    /// selection, add a caret selecting the next occurrence of its text after
    /// the primary, wrapping past the last row to the first and skipping
    /// occurrences already selected.
    SelectNextMatch,
    /// Every occurrence of the primary's selection, or of the word under it,
    /// on every row.
    SelectAllMatches,
    /// One caret, at the primary's head, with nothing selected.
    Collapse,
    /// Swap the primary's row with the one above (below), carrying the caret.
    /// Other carets are dropped first. Reported by [`NamesEditor::take_swap`].
    MoveRowUp,
    MoveRowDown,
    /// Select the whole line at every caret. Not bound to a key.
    SelectLine,
}

/// What one chord means, before it is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Edit(EditorOp),
    Submit,
    /// Collapse if there is anything to collapse, else cancel.
    Escape,
}

/// A caret and the bookkeeping the public [`Cursor`] leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Caret {
    cursor: Cursor,
    /// The column `↑`/`↓` aim for. `None` is "the head's own column", which
    /// is what any horizontal motion or edit resets it to.
    goal: Option<usize>,
    /// When this caret was made. The largest is the primary.
    age: u64,
}

/// Everything an undo step puts back.
#[derive(Debug, Clone)]
struct Snapshot {
    lines: Vec<String>,
    carets: Vec<Caret>,
    /// Which rows were touched, so an undo puts back whose line each one was.
    touched: Vec<bool>,
    /// The rows the step after this snapshot swapped, if it was a row move,
    /// so undoing or redoing it can report the swap again.
    swap: Option<(usize, usize)>,
}

/// One replacement on one row, and the caret that asked for it.
#[derive(Debug, Clone)]
struct Edit {
    row: usize,
    range: Range<usize>,
    text: String,
    /// Index into the carets. An owned edit puts its caret after the text it
    /// inserted; an edit with no owner only shifts the carets around it.
    owner: Option<usize>,
}

/// A row's edits after overlapping ones are merged, with where each landed.
#[derive(Debug)]
struct Span {
    start: usize,
    end: usize,
    text: String,
    owners: Vec<usize>,
    /// Where the replacement starts in the rewritten line, and its length.
    new_start: usize,
    new_len: usize,
}

/// The right column of the bulk-rename card: one line per file, any number
/// of carets.
///
/// Construct with [`NamesEditor::new`], feed it [`Chord`]s, paint from
/// [`NamesEditor::lines`] and [`NamesEditor::cursors_on`].
#[derive(Debug, Clone)]
pub struct NamesEditor {
    lines: Vec<String>,
    /// The truth about the carets, sorted by position and merged.
    carets: Vec<Caret>,
    /// The same carets without the bookkeeping, for [`NamesEditor::cursors`].
    /// Rebuilt by [`NamesEditor::settle`], which every change ends with.
    cursors: Vec<Cursor>,
    /// Index of the youngest caret.
    primary: usize,
    next_age: u64,
    page: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Set while a run of typing is open, so a typed word is one undo step.
    /// Anything that is not typing clears it.
    tagged: bool,
    /// Row swaps not yet collected by [`NamesEditor::take_swap`], oldest first.
    swaps: Vec<(usize, usize)>,
    /// One flag per line: whether a person has changed that row's text since
    /// the template last wrote the column. See the module header.
    touched: Vec<bool>,
    /// Whether the last verb [`NamesEditor::feed`] or [`NamesEditor::apply`]
    /// ran was an undo or a redo. See [`NamesEditor::last_was_history`].
    history: bool,
}

impl NamesEditor {
    /// One caret on row 0, at the stem end of that line: before the last `.`
    /// that is not the first character, or at the end if there is none. The
    /// single-file rename opens on the same rule
    /// ([`crate::input::InputBuffer::for_rename_stem`]).
    pub fn new(lines: Vec<String>) -> NamesEditor {
        let col = lines.first().map_or(0, |line| stem_end(line));
        let touched = vec![false; lines.len()];
        let mut editor = NamesEditor {
            lines,
            carets: vec![Caret {
                cursor: Cursor::caret(Pos { row: 0, col }),
                goal: None,
                age: 0,
            }],
            cursors: Vec::new(),
            primary: 0,
            next_age: 1,
            page: DEFAULT_PAGE,
            undo: Vec::new(),
            redo: Vec::new(),
            tagged: false,
            swaps: Vec::new(),
            touched,
            history: false,
        };
        editor.settle();
        editor
    }

    // ── Accessors: everything the card paints from ──────────────────────────

    /// Every new name, one per row.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn line(&self, row: usize) -> Option<&str> {
        self.lines.get(row).map(String::as_str)
    }

    /// How many rows there are.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether a person has changed row `row`'s text since the template last
    /// wrote the column (see the module header). A row that does not exist
    /// is untouched.
    pub fn touched(&self, row: usize) -> bool {
        self.touched.get(row).copied().unwrap_or(false)
    }

    /// Every row's [`NamesEditor::touched`] flag, in row order.
    pub fn touched_rows(&self) -> &[bool] {
        &self.touched
    }

    /// Whether the last verb [`NamesEditor::feed`] or [`NamesEditor::apply`]
    /// ran was `Undo` or `Redo`.
    ///
    /// An undo can put back lines the card wrote from facts it has since
    /// learned more about, such as a row still waiting on a photo that has
    /// since arrived, or under a template the field no longer says. The card
    /// asks this after each keystroke. When the answer is yes, it resolves the
    /// untouched rows again.
    pub fn last_was_history(&self) -> bool {
        self.history
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Every caret, sorted by position. [`NamesEditor::primary`] is the one
    /// added most recently.
    pub fn cursors(&self) -> &[Cursor] {
        &self.cursors
    }

    /// What a copy takes: every caret's selection, top to bottom, one per
    /// line. Empty when nothing is selected.
    ///
    /// A caret with nothing selected adds nothing, not even an empty line. A
    /// copy at forty carets of which three have selections is three pieces of
    /// text, and a paste of it back into the column gives each caret its own
    /// line only if that count is right.
    pub fn selected_text(&self) -> String {
        self.cursors
            .iter()
            .filter(|cursor| cursor.is_selection())
            .map(|cursor| self.selected_chars(cursor).into_iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The youngest caret: the one `Ctrl+d` searches from, `Alt+↑` moves and
    /// `Esc` keeps.
    pub fn primary(&self) -> Cursor {
        self.cursors[self.primary]
    }

    /// The carets on one row, for painting it.
    pub fn cursors_on(&self, row: usize) -> impl Iterator<Item = &Cursor> {
        self.cursors.iter().filter(move |c| c.head.row == row)
    }

    /// How many rows the card shows, which is how far `PgUp` and `PgDn` move.
    /// Zero is taken as one.
    pub fn set_page(&mut self, rows: usize) {
        self.page = rows.max(1);
    }

    /// The oldest row swap the card has not yet mirrored, as the two rows
    /// whose lines traded places. A swap is its own inverse, so the app runs
    /// `vec.swap(a, b)` on each of its parallel vectors whether the swap came
    /// from `Alt+↑`, `Alt+↓`, an undo or a redo.
    ///
    /// Reading one clears it. One keystroke makes at most one swap, so a card
    /// that reads after every [`NamesEditor::feed`] sees each as it happens;
    /// the swaps queue rather than overwrite so that a card that reads less
    /// often can drain them with `while let` and still end up in step.
    pub fn take_swap(&mut self) -> Option<(usize, usize)> {
        (!self.swaps.is_empty()).then(|| self.swaps.remove(0))
    }

    // ── The dispatcher ──────────────────────────────────────────────────────

    /// Feed one keystroke, through the built-in map in the module header.
    ///
    /// A printable key with no Ctrl, Alt or Super is typed at every caret,
    /// replacing selections. A chord with no binding answers
    /// [`EditorEvent::Ignored`] and changes nothing.
    pub fn feed(&mut self, chord: Chord) -> EditorEvent {
        self.history = false;
        let m = chord.mods;
        if !m.ctrl && !m.alt && !m.super_key {
            if let Some(c) = printable(chord) {
                self.type_text(&c.to_string());
                return EditorEvent::Consumed;
            }
        }
        match binding(chord) {
            Some(Action::Edit(op)) => self.apply(op),
            Some(Action::Submit) => EditorEvent::Submit,
            Some(Action::Escape) => {
                let busy = self.cursors.len() > 1 || self.cursors.iter().any(Cursor::is_selection);
                if busy {
                    self.apply(EditorOp::Collapse)
                } else {
                    EditorEvent::Cancel
                }
            }
            None => EditorEvent::Ignored,
        }
    }

    /// Run one editing verb. Always [`EditorEvent::Consumed`]; a verb with
    /// nothing to do (`↑` on the first row, `Backspace` at column 0) does
    /// nothing and still counts as handled.
    pub fn apply(&mut self, op: EditorOp) -> EditorEvent {
        use EditorOp as O;
        self.tagged = false;
        self.history = matches!(op, O::Undo | O::Redo);
        let last = self.lines.len().saturating_sub(1);
        let page = self.page;
        match op {
            O::MoveLeft => self.slide(false, |_, c| step_left(c, false)),
            O::MoveRight => self.slide(false, |chars, c| step_right(chars, c, false)),
            O::MoveBol => self.slide(false, |_, _| 0),
            O::MoveEol => self.slide(false, |chars, _| chars.len()),
            O::WordForward => self.slide(false, |chars, c| word_forward(chars, c.head.col, kind)),
            O::WordBackward => self.slide(false, |chars, c| word_backward(chars, c.head.col, kind)),
            O::SelectLeft => self.slide(true, |_, c| step_left(c, true)),
            O::SelectRight => self.slide(true, |chars, c| step_right(chars, c, true)),
            O::SelectBol => self.slide(true, |_, _| 0),
            O::SelectEol => self.slide(true, |chars, _| chars.len()),
            O::SelectWordForward => {
                self.slide(true, |chars, c| word_forward(chars, c.head.col, kind))
            }
            O::SelectWordBackward => {
                self.slide(true, |chars, c| word_backward(chars, c.head.col, kind))
            }
            O::SubwordForward => self.slide(false, |chars, c| {
                word_forward(chars, c.head.col, subword_kind)
            }),
            O::SubwordBackward => self.slide(false, |chars, c| {
                word_backward(chars, c.head.col, subword_kind)
            }),
            O::SelectSubwordForward => self.slide(true, |chars, c| {
                word_forward(chars, c.head.col, subword_kind)
            }),
            O::SelectSubwordBackward => self.slide(true, |chars, c| {
                word_backward(chars, c.head.col, subword_kind)
            }),
            O::Backspace => {
                self.delete(|_, c| (c.head.col > 0).then(|| c.head.col - 1..c.head.col))
            }
            O::DeleteUnder => self
                .delete(|chars, c| (c.head.col < chars.len()).then(|| c.head.col..c.head.col + 1)),
            O::KillBol => self.delete(|_, c| Some(0..c.head.col)),
            O::KillEol => self.delete(|chars, c| Some(c.head.col..chars.len())),
            O::KillWordBackward => {
                self.delete(|chars, c| Some(word_backward(chars, c.head.col, kind)..c.head.col))
            }
            O::KillWordForward => {
                self.delete(|chars, c| Some(c.head.col..word_forward(chars, c.head.col, kind)))
            }
            O::Undo => self.undo(),
            O::Redo => self.redo(),
            O::Up => self.climb(|row| row.saturating_sub(1)),
            O::Down => self.climb(|row| row + 1),
            O::PageUp => self.climb(|row| row.saturating_sub(page)),
            O::PageDown => self.climb(|row| row + page),
            O::FirstRow => {
                self.collapse();
                self.climb(|_| 0);
            }
            O::LastRow => {
                self.collapse();
                self.climb(|_| last);
            }
            O::AddCursorAbove => self.add_cursor(false),
            O::AddCursorBelow => self.add_cursor(true),
            O::SelectNextMatch => self.select_next_match(),
            O::SelectAllMatches => self.select_all_matches(),
            O::Collapse => self.collapse(),
            O::MoveRowUp => self.move_row(false),
            O::MoveRowDown => self.move_row(true),
            O::SelectLine => self.select_line(),
        }
        EditorEvent::Consumed
    }

    /// Type `text` at every caret, as a paste. Newlines are dropped, because
    /// a row cannot be split.
    ///
    /// Also the door for an IME commit or a dead-key sequence, which arrive
    /// as composed text rather than as a chord (see
    /// [`crate::input::InputBuffer::insert_text`]). Like the prompt's, it
    /// joins the typing run it arrives in.
    pub fn insert_text(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
        self.type_text(&text);
    }

    /// Replace every line at once, when the template has rewritten the
    /// column. One undo step, and undoing it restores the carets with the
    /// lines. Carets are clamped to the new lengths, except that the primary
    /// moves to its row's stem end if its column is now past the end.
    ///
    /// Every row is untouched afterwards: the template has spoken for all of
    /// them.
    ///
    /// Lines identical to the current ones are not an edit. They leave no undo
    /// step and do not end a typing run. They still clear the touched flags,
    /// because a row whose text is exactly what the template gives it is the
    /// template's row. A card with fewer rows than before pulls carets on the
    /// missing rows up to the last one.
    pub fn set_lines(&mut self, lines: Vec<String>) {
        if lines == self.lines {
            self.touched.fill(false);
            return;
        }
        self.tagged = false;
        let before = self.snapshot(None);
        self.touched = vec![false; lines.len()];
        self.lines = lines;
        self.fit_carets();
        self.push_undo(before);
        self.settle();
    }

    /// Replace every line at once without an undo step, and without ending a
    /// typing run. Carets are clamped as [`NamesEditor::set_lines`] clamps
    /// them, and the touched flags are left alone: the card only rewrites rows
    /// it already considers untouched this way.
    ///
    /// This is for a change nobody made. The photo reader delivers a file's
    /// taken date while the card is open, and the rows that were waiting on it
    /// ("reading photo…") are filled in. That is the machine catching up, not
    /// an edit. If it were a step, `Ctrl+z` would take the date back out and
    /// leave the row waiting on a read that has already finished, and the
    /// person's keystroke would be spent on that. It would also cut a word
    /// being typed in another row into two steps.
    ///
    /// So `Ctrl+z` never takes back only the photo, because the photo is in no
    /// step. An undo that reaches back past it can put back a line written
    /// before the facts arrived. That row is untouched, though, and the card
    /// resolves its untouched rows again after every undo and redo, so the
    /// facts are written back in.
    pub fn set_lines_quietly(&mut self, lines: Vec<String>) {
        if lines == self.lines {
            return;
        }
        // Only a different number of rows moves the flags: new rows are
        // untouched, and the flags of rows that have gone go with them.
        self.touched.resize(lines.len(), false);
        self.lines = lines;
        self.fit_carets();
        self.settle();
    }

    /// Pull every caret onto the lines [`NamesEditor::set_lines`] or
    /// [`NamesEditor::set_lines_quietly`] just put in: onto the last row if
    /// its own row has gone, and inside its line. The primary is the
    /// exception. Past the end of its line, it moves to the stem end, where a
    /// rename caret belongs.
    fn fit_carets(&mut self) {
        let last = self.lines.len().saturating_sub(1);
        let primary = self.primary;
        for (i, caret) in self.carets.iter_mut().enumerate() {
            let row = caret.cursor.head.row.min(last);
            let line = self.lines.get(row).map_or("", String::as_str);
            let len = line.chars().count();
            if i == primary && caret.cursor.head.col > len {
                caret.cursor = Cursor::caret(Pos {
                    row,
                    col: stem_end(line),
                });
                caret.goal = None;
            } else {
                caret.cursor.anchor = Pos {
                    row,
                    col: caret.cursor.anchor.col.min(len),
                };
                caret.cursor.head = Pos {
                    row,
                    col: caret.cursor.head.col.min(len),
                };
            }
        }
    }

    /// Several edits as one undo step, each a (row, char range, replacement).
    /// Ranges on one row must not overlap. Carets on those rows shift by the
    /// rule typing uses (see the module header), so a caret at the end of a
    /// replaced range lands after the replacement and one after the range
    /// moves by the change in length.
    ///
    /// Both ends of a selection are mapped, so a selection that covered a
    /// replaced range afterwards covers the replacement. A range past the end
    /// of its row is clamped to it, and a row that does not exist is skipped.
    pub fn replace_ranges(&mut self, edits: Vec<(usize, Range<usize>, String)>) {
        self.tagged = false;
        let edits = edits
            .into_iter()
            .map(|(row, range, text)| Edit {
                row,
                range: range.start.min(range.end)..range.end,
                text,
                owner: None,
            })
            .collect();
        self.commit(edits, false);
    }

    /// Delete every caret's selection, as one undo step: the second half of a
    /// cut, after the card has copied [`NamesEditor::selected_text`]. Carets
    /// with nothing selected stay where they are. With no selection anywhere
    /// it does nothing and leaves no step.
    pub fn delete_selections(&mut self) {
        self.tagged = false;
        self.delete(|_, _| None);
    }

    /// One caret, on `row` at its stem end: the card's "jump to the next
    /// problem row".
    pub fn go_to_row(&mut self, row: usize) {
        self.tagged = false;
        let row = row.min(self.lines.len().saturating_sub(1));
        let col = self.lines.get(row).map_or(0, |line| stem_end(line));
        self.only(Cursor::caret(Pos { row, col }));
    }

    // ── The pointer ─────────────────────────────────────────────────────────

    /// A click at `at`, already turned from a pixel into a row and a char
    /// boundary by the painter, and clamped here.
    ///
    /// A plain click leaves one caret there. `add` (Alt+click) adds a caret
    /// and makes it the primary, keeping the others. `extend` (Shift+click)
    /// moves the primary's head and keeps its anchor, dropping the other
    /// carets as a plain click would; the head stays on the anchor's row,
    /// because a selection cannot leave it. `add` wins over `extend`.
    pub fn click(&mut self, at: Pos, extend: bool, add: bool) {
        self.tagged = false;
        let at = self.clamp(at);
        if add {
            let age = self.bump();
            self.carets.push(Caret {
                cursor: Cursor::caret(at),
                goal: None,
                age,
            });
            self.settle();
        } else if extend {
            let mut primary = self.carets[self.primary];
            let row = primary.cursor.anchor.row;
            primary.cursor.head = Pos {
                row,
                col: at.col.min(self.line_len(row)),
            };
            primary.goal = None;
            self.carets = vec![primary];
            self.settle();
        } else {
            self.only(Cursor::caret(at));
        }
    }

    /// Select the word under `at`, leaving one caret at its end.
    ///
    /// `at.col` is the character the pointer is on, as it is for
    /// [`segment_at`]: a double click on the right half of a letter is on
    /// that letter. A double click on a separator takes the word to its
    /// right, and one past the end of the row takes the row's last word.
    pub fn double_click(&mut self, at: Pos) {
        self.tagged = false;
        let at = self.clamp(at);
        let line = self.line(at.row).unwrap_or("");
        let run = segment_at(line, at.col, |c| !is_word_char(c));
        self.only(Cursor {
            anchor: Pos {
                row: at.row,
                col: run.start,
            },
            head: Pos {
                row: at.row,
                col: run.end,
            },
        });
    }

    /// A drag from the last click: the primary's head follows the pointer
    /// along the anchor's row, whatever row the pointer is over.
    pub fn drag_to(&mut self, at: Pos) {
        self.tagged = false;
        let caret = &mut self.carets[self.primary];
        let row = caret.cursor.anchor.row;
        caret.cursor.head = Pos { row, col: at.col };
        caret.goal = None;
        self.settle();
    }

    // ── Motions ─────────────────────────────────────────────────────────────

    /// Move every caret along its own row to wherever `target` says, given
    /// that row's chars. With `extend`, the anchor stays and the selection
    /// grows; without it, the selection drops.
    fn slide(&mut self, extend: bool, target: impl Fn(&[char], &Cursor) -> usize) {
        let lines = &self.lines;
        for caret in &mut self.carets {
            let row = caret.cursor.head.row;
            let chars = chars_of(lines, row);
            let col = target(&chars, &caret.cursor).min(chars.len());
            caret.cursor.head = Pos { row, col };
            if !extend {
                caret.cursor.anchor = caret.cursor.head;
            }
            caret.goal = None;
        }
        self.settle();
    }

    /// Move every caret to the row `row_of` names, at its goal column. A
    /// caret that cannot move (`↑` on the first row) keeps its column; either
    /// way its selection drops, because there is no vertical selection.
    fn climb(&mut self, row_of: impl Fn(usize) -> usize) {
        let lines = &self.lines;
        let last = lines.len().saturating_sub(1);
        for caret in &mut self.carets {
            let head = caret.cursor.head;
            let goal = caret.goal.unwrap_or(head.col);
            let row = row_of(head.row).min(last);
            let col = if row == head.row {
                head.col
            } else {
                goal.min(chars_of(lines, row).len())
            };
            caret.cursor = Cursor::caret(Pos { row, col });
            caret.goal = Some(goal);
        }
        self.settle();
    }

    fn select_line(&mut self) {
        let lines = &self.lines;
        for caret in &mut self.carets {
            let row = caret.cursor.head.row;
            let len = chars_of(lines, row).len();
            caret.cursor = Cursor {
                anchor: Pos { row, col: 0 },
                head: Pos { row, col: len },
            };
            caret.goal = None;
        }
        self.settle();
    }

    // ── Carets ──────────────────────────────────────────────────────────────

    /// Zed's `Ctrl+Shift+↑/↓`. The new caret takes the goal column of the
    /// caret it was made from, so a stack built through a short row keeps its
    /// column on the rows after it.
    fn add_cursor(&mut self, below: bool) {
        let source = if below {
            self.carets.last()
        } else {
            self.carets.first()
        };
        let Some(source) = source.copied() else {
            return;
        };
        let head = source.cursor.head;
        let row = if below {
            head.row + 1
        } else {
            let Some(row) = head.row.checked_sub(1) else {
                return;
            };
            row
        };
        if row >= self.lines.len() {
            return;
        }
        let goal = source.goal.unwrap_or(head.col);
        let col = goal.min(self.line_len(row));
        let age = self.bump();
        self.carets.push(Caret {
            cursor: Cursor::caret(Pos { row, col }),
            goal: Some(goal),
            age,
        });
        self.settle();
    }

    /// Zed's `Ctrl+d`. See [`EditorOp::SelectNextMatch`].
    ///
    /// The search starts after the primary rather than after the caret that
    /// sits last in the list. The primary is the match `Ctrl+d` added last,
    /// so repeated presses walk forward through the rows, wrap once, and then
    /// fill in whatever the walk skipped.
    fn select_next_match(&mut self) {
        let primary = self.carets[self.primary];
        let row = primary.cursor.head.row;
        if !primary.cursor.is_selection() {
            let Some(word) = self.word_at(primary.cursor.head) else {
                return;
            };
            let caret = &mut self.carets[self.primary];
            caret.cursor = selecting(row, word);
            caret.goal = None;
            self.settle();
            return;
        }
        let needle = self.selected_chars(&primary.cursor);
        let from = (row, primary.cursor.range().end);
        let mut first = None;
        let mut next = None;
        'rows: for (r, line) in self.lines.iter().enumerate() {
            for found in occurrences(line, &needle) {
                let taken = self.carets.iter().any(|c| {
                    c.cursor.head.row == r
                        && c.cursor.is_selection()
                        && overlaps(&c.cursor.range(), &found)
                });
                if taken {
                    continue;
                }
                if (r, found.start) >= from {
                    next = Some((r, found));
                    break 'rows;
                }
                if first.is_none() {
                    first = Some((r, found));
                }
            }
        }
        let Some((r, found)) = next.or(first) else {
            return;
        };
        let age = self.bump();
        self.carets.push(Caret {
            cursor: selecting(r, found),
            goal: None,
            age,
        });
        self.settle();
    }

    /// Zed's `Ctrl+Shift+l`: every occurrence of the primary's selection, or
    /// of the word under it, becomes a selection, and the other carets go.
    ///
    /// Occurrences on one row are taken left to right without overlapping,
    /// and the primary's own range is always one of them, so the primary
    /// stays where the person is looking.
    fn select_all_matches(&mut self) {
        let primary = self.carets[self.primary];
        let row = primary.cursor.head.row;
        let own = if primary.cursor.is_selection() {
            primary.cursor
        } else {
            match self.word_at(primary.cursor.head) {
                Some(word) => selecting(row, word),
                None => return,
            }
        };
        let own_range = own.range();
        let needle = self.selected_chars(&own);
        let n = needle.len();
        let mut found = Vec::new();
        for (r, line) in self.lines.iter().enumerate() {
            let hay: Vec<char> = line.chars().collect();
            let mut i = 0;
            while i + n <= hay.len() {
                let range = i..i + n;
                let clashes = r == row && overlaps(&range, &own_range);
                if !clashes && hay[range.clone()] == needle[..] {
                    found.push(selecting(r, range));
                    i += n;
                } else {
                    i += 1;
                }
            }
        }
        found.push(own);
        let mut carets = Vec::with_capacity(found.len());
        for cursor in found {
            let age = self.bump();
            carets.push(Caret {
                cursor,
                goal: None,
                age,
            });
        }
        self.carets = carets;
        self.settle();
    }

    /// Drop every caret but the primary, and its selection with them. What
    /// `Esc` does while there is anything to drop.
    fn collapse(&mut self) {
        let primary = self.carets[self.primary];
        self.carets = vec![Caret {
            cursor: Cursor::caret(primary.cursor.head),
            ..primary
        }];
        self.settle();
    }

    /// Replace every caret with one new one.
    fn only(&mut self, cursor: Cursor) {
        let age = self.bump();
        self.carets = vec![Caret {
            cursor,
            goal: None,
            age,
        }];
        self.settle();
    }

    /// `Alt+↑/↓`. One undo step, taken before the other carets drop so that
    /// undoing it brings them back.
    fn move_row(&mut self, down: bool) {
        let primary = self.carets[self.primary];
        let row = primary.cursor.head.row;
        let target = if down {
            Some(row + 1).filter(|&r| r < self.lines.len())
        } else {
            row.checked_sub(1)
        };
        let Some(target) = target else {
            self.carets = vec![primary];
            self.settle();
            return;
        };
        let before = self.snapshot(Some((row, target)));
        self.push_undo(before);
        self.lines.swap(row, target);
        self.touched.swap(row, target);
        let mut moved = primary;
        moved.cursor.anchor.row = target;
        moved.cursor.head.row = target;
        self.carets = vec![moved];
        self.swaps.push((row, target));
        self.settle();
    }

    /// Put every caret in order, merge the ones that coincide, and rebuild
    /// the public view of them. Every change to the carets ends here, which
    /// is why "sorted, never doubled, primary is the youngest" is one rule
    /// rather than a promise every verb has to keep.
    fn settle(&mut self) {
        let lines = &self.lines;
        let last = lines.len().saturating_sub(1);
        for caret in &mut self.carets {
            let row = caret.cursor.head.row.min(last);
            let len = chars_of(lines, row).len();
            caret.cursor.head = Pos {
                row,
                col: caret.cursor.head.col.min(len),
            };
            caret.cursor.anchor = Pos {
                row,
                col: caret.cursor.anchor.col.min(len),
            };
        }
        if self.carets.is_empty() {
            let age = self.bump();
            self.carets.push(Caret {
                cursor: Cursor::caret(Pos { row: 0, col: 0 }),
                goal: None,
                age,
            });
        }
        self.carets.sort_by_key(|c| {
            let range = c.cursor.range();
            (c.cursor.head.row, range.start, range.end)
        });
        let mut merged: Vec<Caret> = Vec::with_capacity(self.carets.len());
        for caret in self.carets.drain(..) {
            match merged.last_mut() {
                Some(prev) if coincide(&prev.cursor, &caret.cursor) => *prev = union(*prev, caret),
                _ => merged.push(caret),
            }
        }
        self.carets = merged;
        self.cursors = self.carets.iter().map(|c| c.cursor).collect();
        self.primary = self
            .carets
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| c.age)
            .map_or(0, |(i, _)| i);
    }

    fn bump(&mut self) -> u64 {
        let age = self.next_age;
        self.next_age += 1;
        age
    }

    /// The word a caret at `at` means. A caret right after a word means that
    /// word, not the separator after it: the card opens every caret at the
    /// stem end, before the `.`, and `Ctrl+d` there must take the stem's last
    /// word rather than the extension.
    fn word_at(&self, at: Pos) -> Option<Range<usize>> {
        let line = self.line(at.row)?;
        let chars: Vec<char> = line.chars().collect();
        let mut col = at.col.min(chars.len());
        let on_word = chars.get(col).is_some_and(|&c| is_word_char(c));
        if col > 0 && !on_word && is_word_char(chars[col - 1]) {
            col -= 1;
        }
        let run = segment_at(line, col, |c| !is_word_char(c));
        (!run.is_empty()).then_some(run)
    }

    fn selected_chars(&self, cursor: &Cursor) -> Vec<char> {
        let chars = chars_of(&self.lines, cursor.head.row);
        let range = cursor.range();
        chars[range.start.min(chars.len())..range.end.min(chars.len())].to_vec()
    }

    fn clamp(&self, at: Pos) -> Pos {
        let row = at.row.min(self.lines.len().saturating_sub(1));
        Pos {
            row,
            col: at.col.min(self.line_len(row)),
        }
    }

    fn line_len(&self, row: usize) -> usize {
        self.line(row).map_or(0, |line| line.chars().count())
    }

    // ── Edits ───────────────────────────────────────────────────────────────

    /// A printable key or a paste, at every caret, replacing selections.
    fn type_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let edits = self
            .carets
            .iter()
            .enumerate()
            .map(|(i, c)| Edit {
                row: c.cursor.head.row,
                range: c.cursor.range(),
                text: text.to_string(),
                owner: Some(i),
            })
            .collect();
        self.commit(edits, true);
    }

    /// A delete at every caret. A selection is what goes when there is one;
    /// otherwise `span` says what the caret deletes on its row, and `None` or
    /// an empty span is nothing (`Backspace` at column 0, which is where a
    /// text editor would join rows).
    fn delete(&mut self, span: impl Fn(&[char], &Cursor) -> Option<Range<usize>>) {
        let lines = &self.lines;
        let edits = self
            .carets
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let range = if c.cursor.is_selection() {
                    c.cursor.range()
                } else {
                    let chars = chars_of(lines, c.cursor.head.row);
                    span(&chars, &c.cursor)?
                };
                (!range.is_empty()).then(|| Edit {
                    row: c.cursor.head.row,
                    range,
                    text: String::new(),
                    owner: Some(i),
                })
            })
            .collect();
        self.commit(edits, false);
    }

    /// Apply `edits` as one undo step, or as part of the open typing run when
    /// `typing` is set and a run is open. An edit list that changes no line is
    /// not a step.
    fn commit(&mut self, edits: Vec<Edit>, typing: bool) {
        let before = (!(typing && self.tagged)).then(|| self.snapshot(None));
        if self.splice(edits) {
            if let Some(before) = before {
                self.push_undo(before);
            }
            self.tagged = typing;
        }
        self.settle();
    }

    /// Apply every edit, a row at a time, and mark each row whose text changed
    /// as touched. Answers whether any line changed.
    fn splice(&mut self, mut edits: Vec<Edit>) -> bool {
        edits.retain(|e| e.row < self.lines.len());
        edits.sort_by_key(|e| (e.row, e.range.start, e.range.end));
        let mut changed = false;
        for row_edits in edits.chunk_by(|a, b| a.row == b.row) {
            if self.splice_row(row_edits) {
                let row = row_edits.first().map(|e| e.row);
                if let Some(flag) = row.and_then(|row| self.touched.get_mut(row)) {
                    *flag = true;
                }
                changed = true;
            }
        }
        changed
    }

    /// One row's edits, sorted by start: merge the ones that overlap, rewrite
    /// the line in one pass, then map every caret on the row through the
    /// result (see the module header for the rule).
    fn splice_row(&mut self, edits: &[Edit]) -> bool {
        let Some(row) = edits.first().map(|e| e.row) else {
            return false;
        };
        let old: Vec<char> = self.lines[row].chars().collect();
        let n = old.len();

        let mut spans: Vec<Span> = Vec::with_capacity(edits.len());
        for edit in edits {
            let end = edit.range.end.min(n);
            let start = edit.range.start.min(end);
            match spans.last_mut() {
                // Only a real overlap merges: two kills at two carets whose
                // spans cross (`Ctrl+u` at columns 3 and 6) are one kill.
                Some(prev) if start < prev.end => {
                    prev.end = prev.end.max(end);
                    prev.text.push_str(&edit.text);
                    prev.owners.extend(edit.owner);
                }
                _ => spans.push(Span {
                    start,
                    end,
                    text: edit.text.clone(),
                    owners: edit.owner.into_iter().collect(),
                    new_start: 0,
                    new_len: 0,
                }),
            }
        }

        let mut out = String::with_capacity(self.lines[row].len());
        let mut from = 0;
        let mut count = 0;
        for span in &mut spans {
            out.extend(&old[from..span.start]);
            count += span.start - from;
            span.new_start = count;
            span.new_len = span.text.chars().count();
            out.push_str(&span.text);
            count += span.new_len;
            from = span.end;
        }
        out.extend(&old[from..]);

        // The carets are sorted by row (every change ends in `settle`, and an
        // edit only moves carets along their own row), so this row's carets
        // are one run found by binary search. Walking all of them for every
        // row would make typing at a caret on each of N rows cost N².
        let first = self.carets.partition_point(|c| c.cursor.head.row < row);
        let end = self.carets.partition_point(|c| c.cursor.head.row <= row);
        for (i, caret) in self.carets.iter_mut().enumerate().take(end).skip(first) {
            match spans.iter().find(|s| s.owners.contains(&i)) {
                Some(span) => {
                    let col = span.new_start + span.new_len;
                    caret.cursor = Cursor::caret(Pos { row, col });
                }
                None => {
                    caret.cursor.anchor.col = map_col(caret.cursor.anchor.col, &spans);
                    caret.cursor.head.col = map_col(caret.cursor.head.col, &spans);
                }
            }
            caret.goal = None;
        }

        let changed = out != self.lines[row];
        self.lines[row] = out;
        changed
    }

    // ── Undo ────────────────────────────────────────────────────────────────

    fn snapshot(&self, swap: Option<(usize, usize)>) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            carets: self.carets.clone(),
            touched: self.touched.clone(),
            swap,
        }
    }

    /// Record the state before an edit. A new edit is a new branch, so the
    /// redo stack goes.
    fn push_undo(&mut self, before: Snapshot) {
        self.undo.push(before);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn undo(&mut self) {
        let Some(prev) = self.undo.pop() else {
            return;
        };
        let now = Snapshot {
            lines: std::mem::replace(&mut self.lines, prev.lines),
            carets: std::mem::replace(&mut self.carets, prev.carets),
            touched: std::mem::replace(&mut self.touched, prev.touched),
            swap: prev.swap,
        };
        self.redo.push(now);
        if self.redo.len() > UNDO_LIMIT {
            self.redo.remove(0);
        }
        self.swaps.extend(prev.swap);
        self.settle();
    }

    fn redo(&mut self) {
        let Some(next) = self.redo.pop() else {
            return;
        };
        let now = Snapshot {
            lines: std::mem::replace(&mut self.lines, next.lines),
            carets: std::mem::replace(&mut self.carets, next.carets),
            touched: std::mem::replace(&mut self.touched, next.touched),
            swap: next.swap,
        };
        self.undo.push(now);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.swaps.extend(next.swap);
        self.settle();
    }
}

// ── The bindings ────────────────────────────────────────────────────────────

/// The built-in map. The prompt's readline set first, then this editor's own
/// keys; see the module header.
fn binding(chord: Chord) -> Option<Action> {
    let m = chord.mods;
    let plain = m.is_none();
    let shift = m == Mods::SHIFT;
    let ctrl = m == Mods::CTRL;
    let alt = m == Mods::ALT;
    let ctrl_shift = m
        == Mods {
            ctrl: true,
            shift: true,
            ..Mods::NONE
        };
    let alt_shift = m
        == Mods {
            alt: true,
            shift: true,
            ..Mods::NONE
        };
    use Action::{Edit, Escape, Submit};
    use EditorOp as O;

    let action = match chord.key {
        // ── Close ───────────────────────────────────────────────────────────
        Key::Escape if plain => Escape,
        // The same keystroke as `Esc` on a terminal, as it is in the prompt.
        // `Ctrl+c` is not: see the module header on copying.
        Key::Char('[') if ctrl => Escape,
        Key::Enter if plain => Submit,

        // ── Words ───────────────────────────────────────────────────────────
        Key::ArrowLeft if ctrl_shift => Edit(O::SelectWordBackward),
        Key::ArrowRight if ctrl_shift => Edit(O::SelectWordForward),
        Key::ArrowLeft if ctrl => Edit(O::WordBackward),
        Key::ArrowRight if ctrl => Edit(O::WordForward),
        Key::Char('b') if alt => Edit(O::WordBackward),
        Key::Char('f') if alt => Edit(O::WordForward),
        // The prompt's smaller word, which stops at `_` too.
        Key::ArrowLeft if alt_shift => Edit(O::SelectSubwordBackward),
        Key::ArrowRight if alt_shift => Edit(O::SelectSubwordForward),
        Key::ArrowLeft if alt => Edit(O::SubwordBackward),
        Key::ArrowRight if alt => Edit(O::SubwordForward),

        // ── Characters ──────────────────────────────────────────────────────
        Key::ArrowLeft if shift => Edit(O::SelectLeft),
        Key::ArrowRight if shift => Edit(O::SelectRight),
        Key::ArrowLeft if plain => Edit(O::MoveLeft),
        Key::ArrowRight if plain => Edit(O::MoveRight),
        Key::Char('b') if ctrl => Edit(O::MoveLeft),
        Key::Char('f') if ctrl => Edit(O::MoveRight),

        // ── Line ends ───────────────────────────────────────────────────────
        Key::Home if shift => Edit(O::SelectBol),
        Key::End if shift => Edit(O::SelectEol),
        Key::Home if plain => Edit(O::MoveBol),
        Key::End if plain => Edit(O::MoveEol),
        Key::Char('a') if ctrl => Edit(O::MoveBol),
        Key::Char('e') if ctrl => Edit(O::MoveEol),

        // ── Rows ────────────────────────────────────────────────────────────
        // Shift adds nothing to a vertical motion: there is no selection
        // across rows to extend.
        Key::ArrowUp if plain || shift => Edit(O::Up),
        Key::ArrowDown if plain || shift => Edit(O::Down),
        Key::PageUp if plain || shift => Edit(O::PageUp),
        Key::PageDown if plain || shift => Edit(O::PageDown),
        Key::Home if ctrl => Edit(O::FirstRow),
        Key::End if ctrl => Edit(O::LastRow),
        Key::ArrowUp if alt => Edit(O::MoveRowUp),
        Key::ArrowDown if alt => Edit(O::MoveRowDown),

        // ── Carets ──────────────────────────────────────────────────────────
        Key::ArrowUp if ctrl_shift => Edit(O::AddCursorAbove),
        Key::ArrowDown if ctrl_shift => Edit(O::AddCursorBelow),
        Key::Char('d') if ctrl => Edit(O::SelectNextMatch),
        Key::Char('l') if ctrl_shift => Edit(O::SelectAllMatches),

        // ── Delete and kill ─────────────────────────────────────────────────
        Key::Backspace if plain => Edit(O::Backspace),
        Key::Char('h') if ctrl => Edit(O::Backspace),
        Key::Delete if plain => Edit(O::DeleteUnder),
        Key::Char('u') if ctrl => Edit(O::KillBol),
        Key::Char('k') if ctrl => Edit(O::KillEol),
        Key::Char('w') if ctrl => Edit(O::KillWordBackward),
        Key::Char('d') if alt => Edit(O::KillWordForward),

        // ── Undo ────────────────────────────────────────────────────────────
        Key::Char('z') if ctrl => Edit(O::Undo),
        Key::Char('z') if ctrl_shift => Edit(O::Redo),
        Key::Char('y') if ctrl => Edit(O::Redo),

        _ => return None,
    };
    Some(action)
}

/// The character a chord types, if it types one. The prompt's rule: a key is
/// stored unshifted, and Shift is applied here.
fn printable(chord: Chord) -> Option<char> {
    match chord.key {
        Key::Space => Some(' '),
        Key::Char(c) if chord.mods.shift => chord.key.shifted_glyph().or(Some(c)),
        Key::Char(c) => Some(c),
        _ => None,
    }
}

// ── Text helpers ────────────────────────────────────────────────────────────

fn chars_of(lines: &[String], row: usize) -> Vec<char> {
    lines
        .get(row)
        .map_or_else(Vec::new, |line| line.chars().collect())
}

/// Where a caret opens on a name: before the extension, or at the end when
/// there is none. A leading dot is part of the name, as in
/// [`crate::input::InputBuffer::for_rename_stem`].
fn stem_end(line: &str) -> usize {
    let chars: Vec<char> = line.chars().collect();
    chars
        .iter()
        .rposition(|&c| c == '.')
        .filter(|&i| i > 0)
        .unwrap_or(chars.len())
}

fn selecting(row: usize, range: Range<usize>) -> Cursor {
    Cursor {
        anchor: Pos {
            row,
            col: range.start,
        },
        head: Pos {
            row,
            col: range.end,
        },
    }
}

/// Every place `needle` occurs in `line`, overlapping ones included, as char
/// ranges.
fn occurrences(line: &str, needle: &[char]) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    let hay: Vec<char> = line.chars().collect();
    hay.windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle)
        .map(|(i, _)| i..i + needle.len())
        .collect()
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Whether two carets, `a` sorted before `b`, are one caret now. See the
/// module header: the same spot, a caret on or inside a selection, or two
/// selections that overlap. Two selections that only touch stay apart.
fn coincide(a: &Cursor, b: &Cursor) -> bool {
    if a.head.row != b.head.row {
        return false;
    }
    let (ra, rb) = (a.range(), b.range());
    if ra.is_empty() || rb.is_empty() {
        rb.start <= ra.end
    } else {
        rb.start < ra.end
    }
}

/// Two coinciding carets as one: their union, facing the way the younger one
/// faced, with its goal and age.
fn union(a: Caret, b: Caret) -> Caret {
    let (ra, rb) = (a.cursor.range(), b.cursor.range());
    let (start, end) = (ra.start.min(rb.start), ra.end.max(rb.end));
    let younger = if a.age >= b.age { a } else { b };
    let row = younger.cursor.head.row;
    let backward = younger.cursor.head.col < younger.cursor.anchor.col;
    let (anchor, head) = if backward { (end, start) } else { (start, end) };
    Caret {
        cursor: Cursor {
            anchor: Pos { row, col: anchor },
            head: Pos { row, col: head },
        },
        ..younger
    }
}

/// Where an old column lands after a row's spans are applied. Before a span
/// (or at the start of a non-empty one) it only moves with the spans before
/// it; at a span's end, or inside it, it lands after the replacement.
fn map_col(col: usize, spans: &[Span]) -> usize {
    let mut out = col;
    for span in spans {
        if span.end <= col {
            out = span.new_start + span.new_len + (col - span.end);
        } else if span.start < col {
            return span.new_start + span.new_len;
        } else {
            break;
        }
    }
    out
}

/// One character left, or the selection's near edge when a selection is live
/// and the motion does not extend it. The prompt's rule.
fn step_left(cursor: &Cursor, extend: bool) -> usize {
    if cursor.is_selection() && !extend {
        cursor.range().start
    } else {
        cursor.head.col.saturating_sub(1)
    }
}

/// One character right, or the selection's far edge. See [`step_left`].
fn step_right(chars: &[char], cursor: &Cursor, extend: bool) -> usize {
    if cursor.is_selection() && !extend {
        cursor.range().end
    } else {
        (cursor.head.col + 1).min(chars.len())
    }
}

/// What class a character belongs to, for word motions. The prompt's classes.
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

/// The class for the smaller word on `Alt+←/→`: [`kind`], with `_` read as a
/// space, as in the prompt.
fn subword_kind(c: char) -> CharKind {
    if c == '_' {
        CharKind::Space
    } else {
        kind(c)
    }
}

/// One word back from `from`: the start of the word the caret is in, or of
/// the one before if it is already at a start. With [`kind`] the prompt's
/// `Alt+b`, with [`subword_kind`] its `Alt+←`.
fn word_backward(chars: &[char], from: usize, kind: fn(char) -> CharKind) -> usize {
    let mut i = from.min(chars.len());
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

/// One word forward: over any space, then past the end of the word after it.
/// With [`kind`] the prompt's `Alt+f` and the span `Alt+d` kills, with
/// [`subword_kind`] its `Alt+→`.
fn word_forward(chars: &[char], from: usize, kind: fn(char) -> CharKind) -> usize {
    let n = chars.len();
    let mut i = from.min(n);
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

/// The editor is a state machine with no I/O, so every binding is driven here
/// as a script of keystrokes against a fixture card, and the assertion is the
/// lines and the carets afterwards. Scripts are spelled the way
/// `keymap.toml` spells chords, as in the prompt's tests: `"cat"`,
/// `"<ctrl+shift+down>"`, `"<end>"`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::parse_chord;

    // ── Driving ─────────────────────────────────────────────────────────────

    /// Bare characters are themselves; anything in angle brackets goes
    /// through the keymap's own parser.
    fn chords(script: &str) -> Vec<Chord> {
        let mut out = Vec::new();
        let mut rest = script;
        while let Some(c) = rest.chars().next() {
            if c == '<' {
                let Some(end) = rest.find('>') else {
                    panic!("unterminated `<` in {script:?}");
                };
                let name = &rest[1..end];
                out.push(parse_chord(name).unwrap_or_else(|e| panic!("{name:?}: {e}")));
                rest = &rest[end + 1..];
            } else {
                out.push(
                    Chord::from_char(c)
                        .unwrap_or_else(|| panic!("{c:?} is not a chord in {script:?}")),
                );
                rest = &rest[c.len_utf8()..];
            }
        }
        out
    }

    /// Feed a script, returning the last event.
    fn run(ed: &mut NamesEditor, script: &str) -> EditorEvent {
        let mut last = EditorEvent::Consumed;
        for chord in chords(script) {
            last = ed.feed(chord);
        }
        last
    }

    fn card(lines: &[&str]) -> NamesEditor {
        NamesEditor::new(lines.iter().map(|l| l.to_string()).collect())
    }

    fn pos(row: usize, col: usize) -> Pos {
        Pos { row, col }
    }

    /// Every caret's head, top to bottom.
    fn heads(ed: &NamesEditor) -> Vec<(usize, usize)> {
        ed.cursors()
            .iter()
            .map(|c| (c.head.row, c.head.col))
            .collect()
    }

    /// Every caret as (row, selected range), top to bottom.
    fn spans(ed: &NamesEditor) -> Vec<(usize, Range<usize>)> {
        ed.cursors()
            .iter()
            .map(|c| (c.head.row, c.range()))
            .collect()
    }

    fn text(ed: &NamesEditor) -> Vec<&str> {
        ed.lines().iter().map(String::as_str).collect()
    }

    /// A card of three names with a caret at column 0 of every row, the way
    /// a person would build it: `Home`, then two `Ctrl+Shift+↓`.
    fn stacked(lines: &[&str]) -> NamesEditor {
        let mut ed = card(lines);
        run(&mut ed, "<home><ctrl+shift+down><ctrl+shift+down>");
        ed
    }

    // ── Opening ─────────────────────────────────────────────────────────────

    #[test]
    fn a_card_opens_with_one_caret_before_the_first_extension() {
        let cases: &[(&str, usize)] = &[
            ("photo.jpg", 5),
            ("README", 6),
            (".bashrc", 7),
            ("archive.tar.gz", 11),
            ("héllo.jpg", 5),
        ];
        for (name, col) in cases {
            let ed = card(&[name, "other.png"]);
            assert_eq!(heads(&ed), vec![(0, *col)], "{name}");
            assert!(!ed.primary().is_selection());
        }
    }

    #[test]
    fn the_accessors_answer_for_the_rows_that_exist() {
        let ed = card(&["a", "b", "c"]);
        assert_eq!(ed.len(), 3);
        assert!(!ed.is_empty());
        assert_eq!(ed.line(1), Some("b"));
        assert_eq!(ed.line(3), None);
        assert_eq!(ed.cursors().len(), 1);
        assert_eq!(ed.primary(), ed.cursors()[0]);
    }

    /// A card with nothing on it is a place every verb can land without
    /// panicking, and none of them invents a row.
    #[test]
    fn an_empty_card_takes_every_key_without_panicking() {
        let mut ed = NamesEditor::new(Vec::new());
        assert!(ed.is_empty());
        run(
            &mut ed,
            "abc<backspace><delete><ctrl+w><up><down><pagedown><ctrl+end><ctrl+shift+down>\
             <ctrl+d><ctrl+shift+l><alt+down><ctrl+z><ctrl+y><esc>",
        );
        ed.insert_text("x");
        ed.set_page(0);
        ed.replace_ranges(vec![(0, 0..1, "y".into())]);
        ed.go_to_row(3);
        ed.click(pos(2, 2), false, false);
        ed.double_click(pos(0, 0));
        ed.drag_to(pos(4, 4));
        ed.apply(EditorOp::SelectLine);
        assert!(ed.lines().is_empty());
        assert_eq!(heads(&ed), vec![(0, 0)]);
        assert_eq!(ed.take_swap(), None);
    }

    // ── Typing at many carets ───────────────────────────────────────────────

    #[test]
    fn typing_at_three_cursors_inserts_at_all_three() {
        let mut ed = stacked(&["cat.jpg", "dog.jpg", "owl.jpg"]);
        assert_eq!(heads(&ed), vec![(0, 0), (1, 0), (2, 0)]);
        assert_eq!(run(&mut ed, "2024-"), EditorEvent::Consumed);
        assert_eq!(
            text(&ed),
            vec!["2024-cat.jpg", "2024-dog.jpg", "2024-owl.jpg"]
        );
        assert_eq!(heads(&ed), vec![(0, 5), (1, 5), (2, 5)]);
    }

    /// The "append to every row" gesture: a stack of carets, `End`, type.
    /// Each caret goes to the end of its own line, whatever its length.
    #[test]
    fn home_and_end_at_several_cursors_go_to_the_ends_of_each_ones_own_line() {
        let mut ed = stacked(&["a.jpg", "longer.jpg", "mid.png"]);
        run(&mut ed, "<end>");
        assert_eq!(heads(&ed), vec![(0, 5), (1, 10), (2, 7)]);
        run(&mut ed, "!");
        assert_eq!(text(&ed), vec!["a.jpg!", "longer.jpg!", "mid.png!"]);

        for script in ["<home>", "<ctrl+a>"] {
            let mut ed = stacked(&["a.jpg", "longer.jpg", "mid.png"]);
            run(&mut ed, "<end>");
            run(&mut ed, script);
            assert_eq!(heads(&ed), vec![(0, 0), (1, 0), (2, 0)], "{script}");
        }
        let mut ed = stacked(&["a.jpg", "longer.jpg", "mid.png"]);
        run(&mut ed, "<ctrl+e>");
        assert_eq!(heads(&ed), vec![(0, 5), (1, 10), (2, 7)]);
    }

    /// Shift-extended motions select at every caret, and typing then
    /// replaces every selection at once.
    #[test]
    fn a_shift_motion_selects_at_every_caret_and_typing_replaces_them_all() {
        let mut ed = stacked(&["IMG_0001.jpg", "IMG_0002.jpg", "IMG_0003.jpg"]);
        run(&mut ed, "<shift+right><shift+right><shift+right>");
        assert_eq!(spans(&ed), vec![(0, 0..3), (1, 0..3), (2, 0..3)]);
        run(&mut ed, "holiday");
        assert_eq!(
            text(&ed),
            vec!["holiday_0001.jpg", "holiday_0002.jpg", "holiday_0003.jpg"]
        );
        assert!(ed.cursors().iter().all(|c| !c.is_selection()));
        assert_eq!(heads(&ed), vec![(0, 7), (1, 7), (2, 7)]);

        // `Shift+End` and a word-wise extension, too.
        let mut ed = stacked(&["ab cd", "ef gh", "ij kl"]);
        run(&mut ed, "<ctrl+shift+right>");
        assert_eq!(spans(&ed), vec![(0, 0..2), (1, 0..2), (2, 0..2)]);
        run(&mut ed, "<shift+end>");
        assert_eq!(spans(&ed), vec![(0, 0..5), (1, 0..5), (2, 0..5)]);
        run(&mut ed, "<left>");
        assert_eq!(
            heads(&ed),
            vec![(0, 0), (1, 0), (2, 0)],
            "Left collapses to the near edge"
        );
    }

    /// Every letter is text on the first press, as in the prompt, and Shift
    /// types the shifted glyph.
    #[test]
    fn every_letter_types_itself_and_shift_types_the_shifted_glyph() {
        let mut ed = card(&[""]);
        run(&mut ed, "idxlq<shift+->A<shift+1><space>");
        assert_eq!(text(&ed), vec!["idxlq_A! "]);
    }

    /// Enter submits the card. There is no newline to insert: a row is a file.
    #[test]
    fn enter_submits_and_never_inserts_a_newline() {
        let mut ed = stacked(&["a", "b", "c"]);
        assert_eq!(run(&mut ed, "<enter>"), EditorEvent::Submit);
        assert_eq!(text(&ed), vec!["a", "b", "c"]);
        assert_eq!(ed.len(), 3);
        // A modified Enter is not the editor's.
        assert_eq!(run(&mut ed, "<shift+enter>"), EditorEvent::Ignored);
    }

    #[test]
    fn a_paste_lands_at_every_caret_with_its_newlines_dropped() {
        let mut ed = stacked(&["a", "b", "c"]);
        ed.insert_text("x\r\ny\n");
        assert_eq!(text(&ed), vec!["xya", "xyb", "xyc"]);
        assert_eq!(ed.len(), 3);
        assert_eq!(heads(&ed), vec![(0, 2), (1, 2), (2, 2)]);
        // Nothing left after the newlines go is not an edit.
        ed.insert_text("\n");
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["a", "b", "c"]);
    }

    // ── Deleting never joins ────────────────────────────────────────────────

    #[test]
    fn backspace_at_column_zero_does_nothing() {
        let mut ed = card(&["first", "second"]);
        ed.go_to_row(1);
        run(&mut ed, "<home><backspace><ctrl+h><ctrl+w><ctrl+u>");
        assert_eq!(text(&ed), vec!["first", "second"]);
        assert_eq!(heads(&ed), vec![(1, 0)]);
        // …and none of that was an edit, so there is nothing to undo.
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["first", "second"]);
    }

    #[test]
    fn delete_at_the_end_of_a_row_does_nothing() {
        let mut ed = card(&["first", "second"]);
        run(&mut ed, "<end><delete><alt+d><ctrl+k>");
        assert_eq!(text(&ed), vec!["first", "second"]);
        assert_eq!(heads(&ed), vec![(0, 5)]);
    }

    /// Carets on rows of their own each delete on their own row, and one at
    /// column 0 sits the keystroke out while the others act.
    #[test]
    fn backspace_at_several_carets_deletes_behind_each_that_can() {
        let mut ed = card(&["abc", "def", "ghi"]);
        ed.click(pos(0, 2), false, false);
        ed.click(pos(1, 0), false, true);
        ed.click(pos(2, 3), false, true);
        run(&mut ed, "<backspace>");
        assert_eq!(text(&ed), vec!["ac", "def", "gh"]);
        assert_eq!(heads(&ed), vec![(0, 1), (1, 0), (2, 2)]);
    }

    #[test]
    fn delete_takes_the_character_under_each_caret() {
        let mut ed = stacked(&["abc", "def", "ghi"]);
        run(&mut ed, "<delete>");
        assert_eq!(text(&ed), vec!["bc", "ef", "hi"]);
    }

    #[test]
    fn the_kill_family_cuts_at_every_caret() {
        let cases: &[(&str, [&str; 2])] = &[
            ("<ctrl+w>", ["the .jpg", "big .png"]),
            // The `.` is a word of its own, as it is in the prompt.
            ("<alt+d>", ["the catjpg", "big dogpng"]),
            ("<ctrl+u>", [".jpg", ".png"]),
            ("<ctrl+k>", ["the cat", "big dog"]),
        ];
        for (script, want) in cases {
            let mut ed = card(&["the cat.jpg", "big dog.png"]);
            run(&mut ed, "<ctrl+shift+down>");
            assert_eq!(heads(&ed), vec![(0, 7), (1, 7)]);
            run(&mut ed, script);
            assert_eq!(text(&ed), want.to_vec(), "{script}");
        }
    }

    /// Two kills whose spans cross on one row are one kill, and the two carets
    /// that made it land on the same spot and become one.
    #[test]
    fn overlapping_kills_on_one_row_merge_their_carets() {
        let mut ed = card(&["abcdefgh"]);
        ed.click(pos(0, 3), false, false);
        ed.click(pos(0, 6), false, true);
        run(&mut ed, "<ctrl+u>");
        assert_eq!(text(&ed), vec!["gh"]);
        assert_eq!(heads(&ed), vec![(0, 0)]);
    }

    /// Two carets on one row: an edit at the first shifts the second by what
    /// it inserted, so each types in its own place.
    #[test]
    fn two_carets_on_one_row_each_type_in_their_own_place() {
        let mut ed = card(&["a-b-c"]);
        ed.click(pos(0, 1), false, false);
        ed.click(pos(0, 3), false, true);
        run(&mut ed, "xy");
        assert_eq!(text(&ed), vec!["axy-bxy-c"]);
        assert_eq!(heads(&ed), vec![(0, 3), (0, 7)]);
        run(&mut ed, "<backspace><backspace><backspace>");
        assert_eq!(text(&ed), vec!["--c"]);
        assert_eq!(heads(&ed), vec![(0, 0), (0, 1)]);
    }

    // ── Rows ────────────────────────────────────────────────────────────────

    #[test]
    fn up_and_down_keep_the_goal_column_across_a_short_row() {
        let mut ed = card(&["abcdefgh", "ab", "abcdefgh"]);
        ed.click(pos(0, 6), false, false);
        run(&mut ed, "<down>");
        assert_eq!(heads(&ed), vec![(1, 2)], "clamped to the short row");
        run(&mut ed, "<down>");
        assert_eq!(heads(&ed), vec![(2, 6)], "back at the goal below it");
        run(&mut ed, "<up><up>");
        assert_eq!(heads(&ed), vec![(0, 6)]);
        // A horizontal motion sets a new goal.
        run(&mut ed, "<down><left><down>");
        assert_eq!(heads(&ed), vec![(2, 1)]);
    }

    /// There is no vertical selection: Shift adds nothing to `↑`/`↓`, and a
    /// selection drops when its caret changes row.
    #[test]
    fn shift_up_and_down_are_plain_up_and_down() {
        let mut ed = card(&["abc", "def"]);
        run(&mut ed, "<home><shift+right><shift+down>");
        assert_eq!(heads(&ed), vec![(1, 1)]);
        assert!(!ed.primary().is_selection());
        run(&mut ed, "<shift+up>");
        assert_eq!(heads(&ed), vec![(0, 1)]);
    }

    #[test]
    fn up_on_the_first_row_and_down_on_the_last_stay_put() {
        let mut ed = card(&["abc", "def"]);
        run(&mut ed, "<up>");
        assert_eq!(heads(&ed), vec![(0, 3)]);
        run(&mut ed, "<down><down><down>");
        assert_eq!(heads(&ed), vec![(1, 3)]);
    }

    /// A stack that runs into the first row folds into one caret there,
    /// rather than leaving two carets on one spot that would type twice.
    #[test]
    fn carets_that_land_on_the_same_spot_merge() {
        let mut ed = stacked(&["abc", "def", "ghi"]);
        run(&mut ed, "<up>");
        assert_eq!(heads(&ed), vec![(0, 0), (1, 0)]);
        run(&mut ed, "<up>");
        assert_eq!(heads(&ed), vec![(0, 0)]);
        run(&mut ed, "x");
        assert_eq!(text(&ed), vec!["xabc", "def", "ghi"]);

        // Two carets on one row sent to its start are one caret.
        let mut ed = card(&["abcdef"]);
        ed.click(pos(0, 2), false, false);
        ed.click(pos(0, 4), false, true);
        run(&mut ed, "<home>");
        assert_eq!(heads(&ed), vec![(0, 0)]);
    }

    #[test]
    fn page_keys_move_by_the_page_the_card_set() {
        let rows: Vec<String> = (0..30).map(|i| format!("file{i:02}.txt")).collect();
        let mut ed = NamesEditor::new(rows);
        ed.set_page(7);
        run(&mut ed, "<pagedown>");
        assert_eq!(ed.primary().head.row, 7);
        run(&mut ed, "<pagedown><pagedown>");
        assert_eq!(ed.primary().head.row, 21);
        run(&mut ed, "<pagedown>");
        assert_eq!(ed.primary().head.row, 28);
        run(&mut ed, "<pagedown>");
        assert_eq!(ed.primary().head.row, 29, "the last row, not past it");
        run(&mut ed, "<pageup>");
        assert_eq!(ed.primary().head.row, 22);
        ed.set_page(100);
        run(&mut ed, "<shift+pageup>");
        assert_eq!(ed.primary().head.row, 0);
        assert_eq!(ed.primary().head.col, 6, "the column came along");
    }

    /// `Ctrl+Home` and `Ctrl+End` leave one caret, as Zed does, on the first
    /// or last row at the primary's column.
    #[test]
    fn ctrl_home_and_ctrl_end_go_to_the_first_and_last_row_with_one_caret() {
        let mut ed = card(&["abcdef", "ab", "abcdef", "abcd"]);
        ed.click(pos(1, 1), false, false);
        run(&mut ed, "<end><ctrl+shift+down>");
        assert_eq!(ed.cursors().len(), 2);
        run(&mut ed, "<ctrl+end>");
        assert_eq!(heads(&ed), vec![(3, 2)]);
        run(&mut ed, "<ctrl+home>");
        assert_eq!(heads(&ed), vec![(0, 2)]);
    }

    #[test]
    fn go_to_row_leaves_one_caret_at_that_rows_stem_end() {
        let mut ed = stacked(&["a.jpg", "photo.png", "c"]);
        ed.go_to_row(1);
        assert_eq!(heads(&ed), vec![(1, 5)]);
        ed.go_to_row(99);
        assert_eq!(heads(&ed), vec![(2, 1)], "past the end is the last row");
    }

    // ── Adding carets ───────────────────────────────────────────────────────

    #[test]
    fn add_cursor_below_clamps_to_a_shorter_row_and_keeps_the_goal_column() {
        let mut ed = card(&["abcdef", "ab", "abcdef", "abcdef"]);
        ed.click(pos(0, 5), false, false);
        run(&mut ed, "<ctrl+shift+down>");
        assert_eq!(heads(&ed), vec![(0, 5), (1, 2)]);
        run(&mut ed, "<ctrl+shift+down>");
        assert_eq!(heads(&ed), vec![(0, 5), (1, 2), (2, 5)]);
        assert_eq!(
            ed.primary().head,
            pos(2, 5),
            "the newest caret is the primary"
        );
        run(&mut ed, "<ctrl+shift+down><ctrl+shift+down>");
        assert_eq!(heads(&ed).len(), 4, "no caret past the last row");
        assert_eq!(heads(&ed), vec![(0, 5), (1, 2), (2, 5), (3, 5)]);
        // The stack moves as one, every caret keeping its own goal: the one
        // clamped on row 1 is back at 5 on row 0 (where it merges with the
        // caret already there), and the one from row 2 clamps on row 1.
        run(&mut ed, "<up>");
        assert_eq!(heads(&ed), vec![(0, 5), (1, 2), (2, 5)]);
    }

    #[test]
    fn add_cursor_above_grows_the_stack_upward() {
        let mut ed = card(&["abcdef", "ab", "abcdef"]);
        ed.click(pos(2, 4), false, false);
        run(&mut ed, "<ctrl+shift+up><ctrl+shift+up>");
        assert_eq!(heads(&ed), vec![(0, 4), (1, 2), (2, 4)]);
        assert_eq!(ed.primary().head, pos(0, 4));
        run(&mut ed, "<ctrl+shift+up>");
        assert_eq!(heads(&ed).len(), 3, "no caret above the first row");
    }

    #[test]
    fn cursors_on_a_row_are_the_carets_that_row_paints() {
        let mut ed = card(&["abcdef", "ghi"]);
        ed.click(pos(0, 1), false, false);
        ed.click(pos(0, 4), false, true);
        ed.click(pos(1, 2), false, true);
        let row0: Vec<usize> = ed.cursors_on(0).map(|c| c.head.col).collect();
        assert_eq!(row0, vec![1, 4]);
        assert_eq!(ed.cursors_on(1).count(), 1);
        assert_eq!(ed.cursors_on(2).count(), 0);
    }

    // ── Ctrl+d and friends ──────────────────────────────────────────────────

    /// `Ctrl+d` with nothing selected takes the word under the caret, and a
    /// caret at the stem end means the stem's last word, not the extension.
    #[test]
    fn ctrl_d_first_selects_the_word_under_the_caret() {
        let mut ed = card(&["trip IMG.jpg", "IMG.png"]);
        assert_eq!(heads(&ed), vec![(0, 8)], "opens at the stem end");
        run(&mut ed, "<ctrl+d>");
        assert_eq!(spans(&ed), vec![(0, 5..8)]);

        // In the middle of a word it is that word.
        let mut ed = card(&["trip IMG.jpg"]);
        ed.click(pos(0, 2), false, false);
        run(&mut ed, "<ctrl+d>");
        assert_eq!(spans(&ed), vec![(0, 0..4)]);

        // Nothing but separators is nothing to select.
        let mut ed = card(&["--.."]);
        run(&mut ed, "<ctrl+d>");
        assert!(!ed.primary().is_selection());
    }

    #[test]
    fn ctrl_d_then_adds_the_next_occurrence_across_rows_and_wraps() {
        let mut ed = card(&["IMG_1.jpg", "x IMG_2.jpg", "IMG_3 IMG.jpg", "none.jpg"]);
        ed.click(pos(1, 3), false, false);
        run(&mut ed, "<ctrl+d>");
        assert_eq!(spans(&ed), vec![(1, 2..7)], "the word is IMG_2");

        // Select just `IMG` to search for it.
        ed.click(pos(1, 2), false, false);
        run(&mut ed, "<shift+right><shift+right><shift+right>");
        assert_eq!(spans(&ed), vec![(1, 2..5)]);
        run(&mut ed, "<ctrl+d>");
        assert_eq!(spans(&ed), vec![(1, 2..5), (2, 0..3)], "the next row");
        assert_eq!(ed.primary().head, pos(2, 3), "the match is the primary");
        run(&mut ed, "<ctrl+d>");
        assert_eq!(
            spans(&ed),
            vec![(1, 2..5), (2, 0..3), (2, 6..9)],
            "later on the same row"
        );
        run(&mut ed, "<ctrl+d>");
        assert_eq!(
            spans(&ed),
            vec![(0, 0..3), (1, 2..5), (2, 0..3), (2, 6..9)],
            "past the last match it wraps to the first row"
        );
        // Every occurrence is taken: another press changes nothing.
        run(&mut ed, "<ctrl+d>");
        assert_eq!(spans(&ed).len(), 4);

        run(&mut ed, "PIC");
        assert_eq!(
            text(&ed),
            vec!["PIC_1.jpg", "x PIC_2.jpg", "PIC_3 PIC.jpg", "none.jpg"]
        );
        assert_eq!(heads(&ed), vec![(0, 3), (1, 5), (2, 3), (2, 9)]);
        // …and one undo puts back the words and the selections on them.
        run(&mut ed, "<ctrl+z>");
        assert_eq!(
            text(&ed),
            vec!["IMG_1.jpg", "x IMG_2.jpg", "IMG_3 IMG.jpg", "none.jpg"]
        );
        assert_eq!(spans(&ed), vec![(0, 0..3), (1, 2..5), (2, 0..3), (2, 6..9)]);
    }

    /// An occurrence someone already selected by hand is skipped, and the
    /// search goes on to the next one.
    #[test]
    fn ctrl_d_skips_occurrences_already_selected() {
        let mut ed = card(&["ab", "ab", "ab", "ab"]);
        // Row 1 selected by a double click, then row 0 by an Alt+click and
        // `Shift+End` (which leaves row 1's selection as it was). Row 0's is
        // the primary.
        ed.double_click(pos(1, 0));
        ed.click(pos(0, 0), false, true);
        run(&mut ed, "<shift+end>");
        assert_eq!(spans(&ed), vec![(0, 0..2), (1, 0..2)]);
        assert_eq!(ed.primary().head.row, 0);

        run(&mut ed, "<ctrl+d>");
        assert_eq!(
            spans(&ed),
            vec![(0, 0..2), (1, 0..2), (2, 0..2)],
            "row 1 is already selected, so the next match is row 2's"
        );
        run(&mut ed, "<ctrl+d>");
        assert_eq!(spans(&ed).len(), 4);
        run(&mut ed, "<ctrl+d>");
        assert_eq!(spans(&ed).len(), 4, "nothing left to add, even wrapping");
    }

    /// Plain text, case and all: `img` is not `IMG`, and a selection of
    /// punctuation searches for punctuation.
    #[test]
    fn ctrl_d_matches_plain_text_case_sensitively() {
        let mut ed = card(&["IMG-a", "img-b", "IMG-c"]);
        ed.double_click(pos(0, 0));
        run(&mut ed, "<ctrl+d>");
        assert_eq!(spans(&ed), vec![(0, 0..3), (2, 0..3)]);

        let mut ed = card(&["a--b", "c--d"]);
        ed.click(pos(0, 1), false, false);
        run(&mut ed, "<shift+right><shift+right><ctrl+d>");
        assert_eq!(spans(&ed), vec![(0, 1..3), (1, 1..3)]);
    }

    #[test]
    fn ctrl_shift_l_selects_every_occurrence_on_every_row() {
        let mut ed = card(&["cat cat.jpg", "dog.jpg", "a cat.png"]);
        ed.click(pos(2, 3), false, false);
        run(&mut ed, "<ctrl+shift+l>");
        assert_eq!(spans(&ed), vec![(0, 0..3), (0, 4..7), (2, 2..5)]);
        assert_eq!(ed.primary().range(), 2..5, "the primary stays where it was");
        assert_eq!(ed.primary().head.row, 2);
        run(&mut ed, "owl");
        assert_eq!(text(&ed), vec!["owl owl.jpg", "dog.jpg", "a owl.png"]);

        // From a selection, it is the selection's text that is searched for,
        // and the other carets go: row 3's `w.jpg` becomes its `.jpg`.
        let mut ed = card(&["x.jpg", "y.jpg", "z.png", "w.jpg"]);
        ed.click(pos(3, 0), false, false);
        ed.click(pos(0, 1), false, true);
        run(&mut ed, "<shift+end>");
        assert_eq!(spans(&ed), vec![(0, 1..5), (3, 0..5)]);
        run(&mut ed, "<ctrl+shift+l>");
        assert_eq!(spans(&ed), vec![(0, 1..5), (1, 1..5), (3, 1..5)]);
        assert_eq!(ed.primary().head.row, 0);
    }

    /// Overlapping occurrences are taken left to right without overlapping,
    /// and the primary's own range is always one of them.
    #[test]
    fn ctrl_shift_l_takes_occurrences_without_overlap_around_the_primary() {
        let mut ed = card(&["aaaa", "aaa"]);
        ed.click(pos(0, 1), false, false);
        run(&mut ed, "<shift+right><shift+right><ctrl+shift+l>");
        assert_eq!(spans(&ed), vec![(0, 1..3), (1, 0..2)]);
        assert_eq!(ed.primary().range(), 1..3);
    }

    // ── Esc ─────────────────────────────────────────────────────────────────

    #[test]
    fn escape_collapses_first_and_cancels_second() {
        let mut ed = stacked(&["a", "b", "c"]);
        run(&mut ed, "<end>");
        assert_eq!(run(&mut ed, "<esc>"), EditorEvent::Consumed);
        assert_eq!(
            heads(&ed),
            vec![(2, 1)],
            "the primary, the newest caret, stays"
        );
        assert_eq!(run(&mut ed, "<esc>"), EditorEvent::Cancel);

        // A single selection is something to collapse too.
        let mut ed = card(&["photo.jpg"]);
        run(&mut ed, "<shift+home>");
        assert_eq!(run(&mut ed, "<esc>"), EditorEvent::Consumed);
        assert_eq!(
            heads(&ed),
            vec![(0, 0)],
            "the caret stays where the head was"
        );
        assert!(!ed.primary().is_selection());
        assert_eq!(run(&mut ed, "<esc>"), EditorEvent::Cancel);

        // Many selections collapse in one press, not one stage per kind.
        let mut ed = stacked(&["ab", "cd", "ef"]);
        run(&mut ed, "<shift+end>");
        assert_eq!(run(&mut ed, "<esc>"), EditorEvent::Consumed);
        assert_eq!(run(&mut ed, "<esc>"), EditorEvent::Cancel);
    }

    #[test]
    fn ctrl_bracket_is_escape() {
        let mut ed = stacked(&["a", "b", "c"]);
        assert_eq!(run(&mut ed, "<ctrl+[>"), EditorEvent::Consumed);
        assert_eq!(ed.cursors().len(), 1);
        assert_eq!(run(&mut ed, "<ctrl+[>"), EditorEvent::Cancel);
    }

    /// `Ctrl+c` and `Ctrl+x` are the card's copy and cut, so the editor
    /// leaves them alone: the carets and their selections are still there
    /// for the card to read.
    #[test]
    fn ctrl_c_and_ctrl_x_are_the_cards_not_escape() {
        for key in ["<ctrl+c>", "<ctrl+x>"] {
            let mut ed = stacked(&["ab", "cd", "ef"]);
            run(&mut ed, "<shift+end>");
            assert_eq!(run(&mut ed, key), EditorEvent::Ignored, "{key}");
            assert_eq!(
                spans(&ed),
                vec![(0, 0..2), (1, 0..2), (2, 0..2)],
                "{key} collapsed nothing"
            );
            assert_eq!(text(&ed), vec!["ab", "cd", "ef"], "{key} cut nothing");
        }
    }

    // ── Copy and cut ────────────────────────────────────────────────────────

    /// The selections top to bottom, one to a line, and nothing for a caret
    /// that has nothing selected.
    #[test]
    fn selected_text_is_every_selection_in_row_order() {
        let mut ed = card(&["cat.jpg", "dog.jpg", "owl.jpg"]);
        assert_eq!(ed.selected_text(), "", "a bare caret selects nothing");

        // Built bottom-up, so the primary is the top row: the order is the
        // rows', not the carets' ages.
        ed.click(pos(2, 0), false, false);
        run(
            &mut ed,
            "<ctrl+shift+up><ctrl+shift+up><shift+right><shift+right>",
        );
        assert_eq!(ed.selected_text(), "ca\ndo\now");

        // Two on one row, left to right, and a caret with no selection adds
        // no empty line.
        let mut ed = card(&["ab cd ef", "gh"]);
        ed.click(pos(0, 3), false, false);
        ed.click(pos(0, 0), false, true);
        run(&mut ed, "<shift+right><shift+right>");
        ed.click(pos(1, 1), false, true);
        assert_eq!(ed.cursors().len(), 3);
        assert_eq!(ed.selected_text(), "ab\ncd");
    }

    /// A cut's delete takes every selection in one undo step and leaves bare
    /// carets alone. With nothing selected it is no edit at all.
    #[test]
    fn delete_selections_is_one_step_and_nothing_without_a_selection() {
        let mut ed = stacked(&["cat.jpg", "dog.jpg", "owl.jpg"]);
        run(&mut ed, "<shift+right><shift+right><shift+right>");
        ed.delete_selections();
        assert_eq!(text(&ed), vec![".jpg", ".jpg", ".jpg"]);
        assert_eq!(heads(&ed), vec![(0, 0), (1, 0), (2, 0)]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["cat.jpg", "dog.jpg", "owl.jpg"]);
        assert_eq!(
            spans(&ed),
            vec![(0, 0..3), (1, 0..3), (2, 0..3)],
            "the selections come back with the text"
        );

        let mut ed = card(&["cat.jpg"]);
        run(&mut ed, "x");
        ed.delete_selections();
        assert_eq!(text(&ed), vec!["catx.jpg"]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(
            text(&ed),
            vec!["cat.jpg"],
            "no step was left where there was nothing to cut"
        );
    }

    // ── What the card keeps for itself ──────────────────────────────────────

    #[test]
    fn tab_and_ctrl_arrows_up_and_down_are_the_cards_not_the_editors() {
        let mut ed = card(&["a", "b"]);
        for key in [
            "<tab>",
            "<shift+tab>",
            "<ctrl+up>",
            "<ctrl+down>",
            "<ctrl+q>",
            "<f2>",
            "<ctrl+alt+x>",
        ] {
            assert_eq!(run(&mut ed, key), EditorEvent::Ignored, "{key}");
        }
        assert_eq!(text(&ed), vec!["a", "b"]);
        assert_eq!(heads(&ed), vec![(0, 1)]);
    }

    /// `Ctrl+d` means select-next here, not the prompt's delete-under-caret;
    /// `Delete` still deletes.
    #[test]
    fn ctrl_d_selects_rather_than_deletes() {
        let mut ed = card(&["ab cd"]);
        ed.click(pos(0, 0), false, false);
        run(&mut ed, "<ctrl+d>");
        assert_eq!(text(&ed), vec!["ab cd"]);
        assert_eq!(spans(&ed), vec![(0, 0..2)]);
        run(&mut ed, "<left><delete>");
        assert_eq!(text(&ed), vec!["b cd"]);
    }

    // ── Moving rows ─────────────────────────────────────────────────────────

    #[test]
    fn move_row_down_swaps_lines_carries_the_caret_and_reports_the_swap() {
        let mut ed = card(&["one.txt", "two.txt", "three.txt"]);
        run(&mut ed, "<ctrl+shift+down><ctrl+shift+down>");
        ed.go_to_row(0);
        run(&mut ed, "<shift+home>");
        assert_eq!(run(&mut ed, "<alt+down>"), EditorEvent::Consumed);
        assert_eq!(text(&ed), vec!["two.txt", "one.txt", "three.txt"]);
        assert_eq!(
            spans(&ed),
            vec![(1, 0..3)],
            "the selection travels with the row"
        );
        assert_eq!(ed.take_swap(), Some((0, 1)));
        assert_eq!(ed.take_swap(), None, "reading it clears it");

        run(&mut ed, "<alt+down>");
        assert_eq!(text(&ed), vec!["two.txt", "three.txt", "one.txt"]);
        assert_eq!(ed.take_swap(), Some((1, 2)));
        // The last row has nowhere to go.
        run(&mut ed, "<alt+down>");
        assert_eq!(text(&ed), vec!["two.txt", "three.txt", "one.txt"]);
        assert_eq!(ed.take_swap(), None);

        run(&mut ed, "<alt+up>");
        assert_eq!(text(&ed), vec!["two.txt", "one.txt", "three.txt"]);
        assert_eq!(ed.primary().head.row, 1);
        assert_eq!(ed.take_swap(), Some((2, 1)));
    }

    /// Other carets are dropped before the move, so only the primary's row
    /// moves.
    #[test]
    fn move_row_drops_the_other_carets_first() {
        let mut ed = stacked(&["a", "b", "c"]);
        run(&mut ed, "<alt+up>");
        assert_eq!(text(&ed), vec!["a", "c", "b"]);
        assert_eq!(heads(&ed), vec![(1, 0)]);
        assert_eq!(ed.take_swap(), Some((2, 1)));
        // Undo puts the row back and brings the dropped carets with it.
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["a", "b", "c"]);
        assert_eq!(heads(&ed), vec![(0, 0), (1, 0), (2, 0)]);
        assert_eq!(ed.take_swap(), Some((2, 1)));
    }

    /// The app mirrors every swap in its parallel vectors, so undoing a swap
    /// has to tell it to swap back, and redoing it to swap again.
    #[test]
    fn undoing_and_redoing_a_row_move_report_the_swap_again() {
        let mut ed = card(&["a", "b", "c"]);
        let mut mirror = vec!["A", "B", "C"];
        let follow = |ed: &mut NamesEditor, mirror: &mut Vec<&str>| {
            while let Some((a, b)) = ed.take_swap() {
                mirror.swap(a, b);
            }
        };
        run(&mut ed, "<alt+down><alt+down>");
        follow(&mut ed, &mut mirror);
        assert_eq!(text(&ed), vec!["b", "c", "a"]);
        assert_eq!(mirror, vec!["B", "C", "A"]);

        run(&mut ed, "<ctrl+z>");
        follow(&mut ed, &mut mirror);
        assert_eq!(text(&ed), vec!["b", "a", "c"]);
        assert_eq!(mirror, vec!["B", "A", "C"]);

        run(&mut ed, "<ctrl+z>");
        follow(&mut ed, &mut mirror);
        assert_eq!(text(&ed), vec!["a", "b", "c"]);
        assert_eq!(mirror, vec!["A", "B", "C"]);

        run(&mut ed, "<ctrl+y><ctrl+shift+z>");
        follow(&mut ed, &mut mirror);
        assert_eq!(text(&ed), vec!["b", "c", "a"]);
        assert_eq!(mirror, vec!["B", "C", "A"]);

        // An undo of an ordinary edit reports no swap.
        run(&mut ed, "x<ctrl+z>");
        assert_eq!(ed.take_swap(), None);
    }

    // ── Undo ────────────────────────────────────────────────────────────────

    #[test]
    fn undo_takes_back_a_typed_word_in_one_step() {
        let mut ed = stacked(&["a", "b", "c"]);
        run(&mut ed, "new-");
        assert_eq!(text(&ed), vec!["new-a", "new-b", "new-c"]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["a", "b", "c"]);
        assert_eq!(
            heads(&ed),
            vec![(0, 0), (1, 0), (2, 0)],
            "the carets come back too"
        );
        run(&mut ed, "<ctrl+y>");
        assert_eq!(text(&ed), vec!["new-a", "new-b", "new-c"]);
        assert_eq!(heads(&ed), vec![(0, 4), (1, 4), (2, 4)]);
    }

    /// Any motion or command ends a typing run, so the next word is its own
    /// step.
    #[test]
    fn a_motion_ends_the_typing_run() {
        let mut ed = card(&[""]);
        run(&mut ed, "ab<left>cd");
        assert_eq!(text(&ed), vec!["acdb"]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["ab"]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec![""]);
        run(&mut ed, "<ctrl+z><ctrl+z>");
        assert_eq!(text(&ed), vec![""], "past the bottom is a no-op");

        // A delete ends it too.
        let mut ed = card(&[""]);
        run(&mut ed, "ab<backspace>cd");
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["a"]);
    }

    #[test]
    fn a_paste_joins_the_typing_run_it_arrives_in() {
        let mut ed = card(&[""]);
        run(&mut ed, "a");
        ed.insert_text("私");
        run(&mut ed, "b");
        assert_eq!(text(&ed), vec!["a私b"]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec![""]);
    }

    #[test]
    fn typing_after_an_undo_drops_the_redo() {
        let mut ed = card(&[""]);
        run(&mut ed, "cat<ctrl+z>dog<ctrl+y>");
        assert_eq!(text(&ed), vec!["dog"]);
    }

    #[test]
    fn set_lines_is_one_undo_step_that_restores_the_lines_and_the_carets() {
        let mut ed = card(&["cat.jpg", "dog.jpg", "owl.jpg"]);
        run(&mut ed, "<ctrl+shift+down><ctrl+shift+down>");
        assert_eq!(heads(&ed), vec![(0, 3), (1, 3), (2, 3)]);
        ed.set_lines(vec![
            "cat-01.jpg".into(),
            "dog-02.jpg".into(),
            "owl-03.jpg".into(),
        ]);
        assert_eq!(text(&ed), vec!["cat-01.jpg", "dog-02.jpg", "owl-03.jpg"]);
        assert_eq!(
            heads(&ed),
            vec![(0, 3), (1, 3), (2, 3)],
            "columns still fit"
        );
        run(&mut ed, "<end>");

        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["cat.jpg", "dog.jpg", "owl.jpg"]);
        assert_eq!(heads(&ed), vec![(0, 3), (1, 3), (2, 3)]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["cat.jpg", "dog.jpg", "owl.jpg"], "one step");
        run(&mut ed, "<ctrl+y>");
        assert_eq!(text(&ed), vec!["cat-01.jpg", "dog-02.jpg", "owl-03.jpg"]);

        // The same lines again are no edit at all.
        ed.set_lines(ed.lines().to_vec());
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["cat.jpg", "dog.jpg", "owl.jpg"]);
    }

    /// Lines shorter than the carets pull them in: the primary to its row's
    /// stem end, where a rename caret belongs, and the rest to the end.
    #[test]
    fn set_lines_moves_a_primary_past_the_end_to_the_stem_end() {
        let mut ed = card(&["long-name.jpg", "long-name.jpg"]);
        run(&mut ed, "<end><ctrl+shift+down>");
        assert_eq!(ed.primary().head, pos(1, 13));
        ed.set_lines(vec!["a.jpg".into(), "b.jpg".into()]);
        assert_eq!(heads(&ed), vec![(0, 5), (1, 1)]);
        assert_eq!(ed.primary().head, pos(1, 1));

        // Fewer rows pull carets up to the last one.
        let mut ed = card(&["a", "b", "c"]);
        ed.go_to_row(2);
        ed.set_lines(vec!["x".into()]);
        assert_eq!(heads(&ed), vec![(0, 1)]);
    }

    /// Lines that are already there are no edit, so they do not end the word
    /// being typed either.
    #[test]
    fn set_lines_with_the_same_lines_leaves_the_typing_run_open() {
        let mut ed = card(&["cat.jpg", "dog.jpg"]);
        run(&mut ed, "ab");
        ed.set_lines(ed.lines().to_vec());
        run(&mut ed, "cd");
        assert_eq!(text(&ed), vec!["catabcd.jpg", "dog.jpg"]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["cat.jpg", "dog.jpg"], "one step");
    }

    /// A quiet rewrite is nobody's edit: no undo step, and the word being
    /// typed when it lands stays one step.
    #[test]
    fn set_lines_quietly_leaves_no_step_and_keeps_the_typing_run() {
        let mut ed = card(&["cat.jpg", "a.jpg"]);
        run(&mut ed, "ab");
        ed.set_lines_quietly(vec!["catab.jpg".into(), "2024-05-06.jpg".into()]);
        assert_eq!(text(&ed), vec!["catab.jpg", "2024-05-06.jpg"]);
        assert_eq!(heads(&ed), vec![(0, 5)], "the caret did not move");
        run(&mut ed, "cd");
        assert_eq!(text(&ed), vec!["catabcd.jpg", "2024-05-06.jpg"]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed)[0], "cat.jpg", "the word came back out whole");
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed)[0], "cat.jpg", "and there was no step under it");

        // On a card with no history at all, nothing to undo either.
        let mut ed = card(&["a.jpg"]);
        ed.set_lines_quietly(vec!["2024-05-06.jpg".into()]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["2024-05-06.jpg"]);
    }

    /// The carets fit the new lines by `set_lines`'s rule: the primary past
    /// the end goes to its stem end, the rest are clamped, and rows that have
    /// gone pull their carets up to the last one.
    #[test]
    fn set_lines_quietly_clamps_the_carets_as_set_lines_does() {
        let mut ed = card(&["long-name.jpg", "long-name.jpg"]);
        run(&mut ed, "<end><ctrl+shift+down>");
        ed.set_lines_quietly(vec!["a.jpg".into(), "b.jpg".into()]);
        assert_eq!(heads(&ed), vec![(0, 5), (1, 1)]);
        assert_eq!(ed.primary().head, pos(1, 1));

        let mut ed = card(&["a", "b", "c"]);
        ed.go_to_row(2);
        ed.set_lines_quietly(vec!["x".into()]);
        assert_eq!(heads(&ed), vec![(0, 1)]);
    }

    // ── Touched rows ────────────────────────────────────────────────────────

    fn flags(ed: &NamesEditor) -> Vec<bool> {
        ed.touched_rows().to_vec()
    }

    /// Typing touches the row it lands on and no other. A motion touches
    /// nothing, and neither does a delete with nothing to delete. Undo puts
    /// the row back as untouched, and redo touches it again.
    #[test]
    fn a_typed_row_is_touched_and_undo_untouches_it() {
        let mut ed = card(&["cat.jpg", "dog.jpg", "owl.jpg"]);
        assert_eq!(flags(&ed), vec![false; 3], "a new card is untouched");
        run(&mut ed, "<down><end><home><ctrl+right><up><down>");
        assert_eq!(flags(&ed), vec![false; 3], "motions touch nothing");
        run(&mut ed, "<home><backspace>");
        assert_eq!(flags(&ed), vec![false; 3], "a delete that deletes nothing");

        run(&mut ed, "x");
        assert_eq!(flags(&ed), vec![false, true, false]);
        assert!(ed.touched(1));
        assert!(!ed.touched(7), "a row that does not exist");
        run(&mut ed, "<ctrl+z>");
        assert_eq!(flags(&ed), vec![false; 3]);
        run(&mut ed, "<ctrl+y>");
        assert_eq!(flags(&ed), vec![false, true, false]);
    }

    /// Every way a person changes a row's text touches it: a paste, a kill, a
    /// cut and a completion, and only on the rows whose text changed.
    #[test]
    fn every_edit_that_changes_a_row_touches_it() {
        let mut ed = card(&["a", "b", "c", "d"]);
        ed.insert_text("x");
        assert_eq!(flags(&ed), vec![true, false, false, false], "a paste");

        ed.go_to_row(1);
        run(&mut ed, "<ctrl+u>");
        assert!(ed.touched(1), "a kill");

        ed.go_to_row(2);
        run(&mut ed, "<shift+home>");
        ed.delete_selections();
        assert!(ed.touched(2), "a cut");

        ed.replace_ranges(vec![(3, 0..1, "d".into()), (3, 1..1, "!".into())]);
        assert!(ed.touched(3), "a completion");
        let mut ed = card(&["a", "b"]);
        ed.replace_ranges(vec![(0, 0..1, "a".into()), (1, 0..1, "z".into())]);
        assert_eq!(
            flags(&ed),
            vec![false, true],
            "a replacement that changes nothing touches nothing"
        );
    }

    /// The template's rewrite gives every row back to the template, even
    /// when it writes exactly the text a row already had, and undoing it
    /// brings back which rows had been typed in.
    #[test]
    fn set_lines_clears_every_flag_and_undo_restores_them() {
        let mut ed = card(&["cat.jpg", "dog.jpg"]);
        run(&mut ed, "x");
        assert_eq!(flags(&ed), vec![true, false]);
        ed.set_lines(vec!["a.jpg".into(), "b.jpg".into()]);
        assert_eq!(flags(&ed), vec![false, false]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["catx.jpg", "dog.jpg"]);
        assert_eq!(flags(&ed), vec![true, false]);

        // The same lines are no step, and still the template's.
        ed.set_lines(ed.lines().to_vec());
        assert_eq!(flags(&ed), vec![false, false]);
    }

    /// A quiet rewrite is the card catching up on rows it already owns, so it
    /// leaves every flag where it was.
    #[test]
    fn set_lines_quietly_leaves_the_flags_alone() {
        let mut ed = card(&["a.jpg", "b.jpg"]);
        ed.go_to_row(1);
        run(&mut ed, "x");
        assert_eq!(flags(&ed), vec![false, true]);
        ed.set_lines_quietly(vec!["2024-05-06.jpg".into(), "bx.jpg".into()]);
        assert_eq!(flags(&ed), vec![false, true]);
    }

    /// A moved row takes its flag with it, and undoing the move brings the
    /// flag back with the row.
    #[test]
    fn a_row_move_carries_its_flag_and_undo_carries_it_back() {
        let mut ed = card(&["a", "b", "c"]);
        run(&mut ed, "x");
        assert_eq!(flags(&ed), vec![true, false, false]);
        run(&mut ed, "<alt+down><alt+down>");
        assert_eq!(text(&ed), vec!["b", "c", "ax"]);
        assert_eq!(flags(&ed), vec![false, false, true]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(flags(&ed), vec![false, true, false]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(flags(&ed), vec![true, false, false]);
        run(&mut ed, "<ctrl+y>");
        assert_eq!(flags(&ed), vec![false, true, false]);
    }

    /// The card asks after each keystroke whether it was an undo or a redo.
    /// Anything else, including a key the editor ignores, answers no.
    #[test]
    fn last_was_history_is_the_last_keystroke_only() {
        let mut ed = card(&["a"]);
        assert!(!ed.last_was_history());
        run(&mut ed, "x<ctrl+z>");
        assert!(ed.last_was_history());
        run(&mut ed, "<left>");
        assert!(!ed.last_was_history());
        run(&mut ed, "<ctrl+y>");
        assert!(ed.last_was_history());
        run(&mut ed, "<tab>");
        assert!(!ed.last_was_history(), "an ignored key");
        run(&mut ed, "<ctrl+shift+z>");
        assert!(ed.last_was_history(), "the other redo");
        run(&mut ed, "y");
        assert!(!ed.last_was_history(), "typing");
    }

    #[test]
    fn replace_ranges_shifts_carets_around_what_it_replaced() {
        let mut ed = card(&["{da}-x-y", "{da}"]);
        // Row 0: one caret at the end of `{da`, one after the replaced range,
        // one before it. Row 1: one at the end of `{da`.
        ed.click(pos(0, 3), false, false);
        ed.click(pos(0, 6), false, true);
        ed.click(pos(0, 0), false, true);
        ed.click(pos(1, 3), false, true);
        ed.replace_ranges(vec![(0, 0..3, "{date".into()), (1, 0..3, "{date".into())]);
        assert_eq!(text(&ed), vec!["{date}-x-y", "{date}"]);
        assert_eq!(
            heads(&ed),
            vec![(0, 0), (0, 5), (0, 8), (1, 5)],
            "before stays, at the end lands after, after shifts by 2"
        );

        // A caret inside a replaced range lands after it too, and so does one
        // at an empty range's spot.
        let mut ed = card(&["abcdef"]);
        ed.click(pos(0, 2), false, false);
        ed.click(pos(0, 5), false, true);
        ed.replace_ranges(vec![(0, 1..4, "X".into()), (0, 5..5, "--".into())]);
        assert_eq!(text(&ed), vec!["aXe--f"]);
        assert_eq!(heads(&ed), vec![(0, 2), (0, 5)]);
    }

    #[test]
    fn replace_ranges_is_one_undo_step_over_every_row() {
        let mut ed = card(&["a", "b", "c"]);
        ed.replace_ranges(vec![
            (0, 0..1, "x".into()),
            (1, 0..1, "y".into()),
            (2, 1..1, "z".into()),
            // A row that does not exist is skipped, not a panic.
            (9, 0..1, "?".into()),
        ]);
        assert_eq!(text(&ed), vec!["x", "y", "cz"]);
        run(&mut ed, "<ctrl+z>");
        assert_eq!(text(&ed), vec!["a", "b", "c"]);
    }

    // ── Unicode ─────────────────────────────────────────────────────────────

    #[test]
    fn a_non_ascii_line_moves_by_chars() {
        let mut ed = card(&["héllo wörld.日本", "日本語.txt"]);
        assert_eq!(heads(&ed), vec![(0, 11)], "the stem end counts chars");
        run(&mut ed, "<left><left>");
        assert_eq!(heads(&ed), vec![(0, 9)]);
        run(&mut ed, "<ctrl+left>");
        assert_eq!(heads(&ed), vec![(0, 6)], "back to the start of wörld");
        run(&mut ed, "<alt+b>");
        assert_eq!(heads(&ed), vec![(0, 0)]);
        run(&mut ed, "<alt+f><ctrl+right>");
        assert_eq!(heads(&ed), vec![(0, 11)]);
        run(&mut ed, "<ctrl+right><ctrl+right>");
        assert_eq!(heads(&ed), vec![(0, 14)], "日本 is one word");
        run(&mut ed, "<down>");
        assert_eq!(
            heads(&ed),
            vec![(1, 7)],
            "the goal clamps to 日本語.txt's 7 chars"
        );
        run(&mut ed, "<home><right><delete>");
        ed.insert_text("é");
        assert_eq!(text(&ed), vec!["héllo wörld.日本", "日é語.txt"]);

        let mut ed = card(&["naïve_café.jpg"]);
        run(&mut ed, "<ctrl+w>");
        assert_eq!(text(&ed), vec![".jpg"], "`_` joins a word");
    }

    /// Every stop `script` makes on `naïve_café_photo` from column `from`,
    /// pressed until the caret stops moving.
    ///
    /// ```text
    /// n a ï v e _ c a f é _  p  h  o  t  o
    /// 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15   (16 is the end)
    /// ```
    fn stops(script: &str, from: usize) -> Vec<usize> {
        let mut ed = card(&["naïve_café_photo"]);
        ed.click(pos(0, from), false, false);
        let mut out = Vec::new();
        loop {
            let before = ed.primary().head.col;
            run(&mut ed, script);
            let after = ed.primary().head.col;
            if after == before {
                return out;
            }
            out.push(after);
        }
    }

    /// `Alt+←/→` step by the prompt's smaller word, reading `_` as a space:
    /// one stop per word. `Ctrl+←/→` and `Alt+b/f` take the whole name in one
    /// press.
    #[test]
    fn alt_arrows_stop_at_the_underscores_and_ctrl_arrows_do_not() {
        assert_eq!(stops("<alt+right>", 0), [5, 10, 16]);
        assert_eq!(stops("<alt+left>", 16), [11, 6, 0]);
        for script in ["<ctrl+right>", "<alt+f>"] {
            assert_eq!(stops(script, 0), [16], "{script}");
        }
        for script in ["<ctrl+left>", "<alt+b>"] {
            assert_eq!(stops(script, 16), [0], "{script}");
        }
    }

    /// Shift on the smaller word selects by it, at every caret: a stack down
    /// a column of snake-case names takes the same part of each.
    #[test]
    fn alt_shift_arrows_select_up_to_the_underscores_at_every_caret() {
        let mut ed = stacked(&["naïve_café_photo", "big_red_car", "x"]);
        run(&mut ed, "<alt+shift+right>");
        assert_eq!(spans(&ed), vec![(0, 0..5), (1, 0..3), (2, 0..1)]);
        run(&mut ed, "<alt+shift+right>");
        assert_eq!(spans(&ed), vec![(0, 0..10), (1, 0..7), (2, 0..1)]);

        let mut ed = card(&["naïve_café_photo"]);
        run(&mut ed, "<end><alt+shift+left>");
        assert_eq!(spans(&ed), vec![(0, 11..16)]);
        assert_eq!(heads(&ed), vec![(0, 11)]);

        let mut ed = card(&["naïve_café_photo"]);
        run(&mut ed, "<home><ctrl+shift+right>");
        assert_eq!(spans(&ed), vec![(0, 0..16)], "the larger word is the name");
    }

    // ── The pointer ─────────────────────────────────────────────────────────

    #[test]
    fn a_click_leaves_one_caret_and_alt_click_adds_one() {
        let mut ed = stacked(&["abc", "def", "ghi"]);
        ed.click(pos(1, 2), false, false);
        assert_eq!(heads(&ed), vec![(1, 2)]);
        ed.click(pos(0, 1), false, true);
        assert_eq!(heads(&ed), vec![(0, 1), (1, 2)]);
        assert_eq!(
            ed.primary().head,
            pos(0, 1),
            "the added caret is the primary"
        );
        // A click past the end of the card is its last row's end.
        ed.click(pos(9, 9), false, false);
        assert_eq!(heads(&ed), vec![(2, 3)]);
        // An Alt+click on an existing caret does not double it.
        ed.click(pos(2, 3), false, true);
        assert_eq!(heads(&ed), vec![(2, 3)]);
    }

    #[test]
    fn a_shift_click_extends_the_primary_on_its_own_row() {
        let mut ed = card(&["abcdef", "ghijkl"]);
        ed.click(pos(0, 2), false, false);
        ed.click(pos(0, 5), true, false);
        assert_eq!(spans(&ed), vec![(0, 2..5)]);
        assert_eq!(ed.primary().head, pos(0, 5));
        // On another row, it stays on the anchor's row.
        ed.click(pos(1, 1), true, false);
        assert_eq!(spans(&ed), vec![(0, 1..2)]);
        assert_eq!(ed.primary().head, pos(0, 1));

        // Like a plain click, it leaves one caret.
        let mut ed = card(&["abcdef", "ghijkl"]);
        run(&mut ed, "<home><ctrl+shift+down>");
        assert_eq!(ed.primary().head, pos(1, 0));
        ed.click(pos(1, 3), true, false);
        assert_eq!(spans(&ed), vec![(1, 0..3)]);
    }

    #[test]
    fn a_drag_grows_the_selection_along_the_row_it_started_on() {
        let mut ed = card(&["abcdef", "ghijkl"]);
        ed.click(pos(0, 2), false, false);
        ed.drag_to(pos(0, 4));
        assert_eq!(spans(&ed), vec![(0, 2..4)]);
        ed.drag_to(pos(1, 6));
        assert_eq!(
            spans(&ed),
            vec![(0, 2..6)],
            "the row clamps to the anchor's"
        );
        ed.drag_to(pos(0, 99));
        assert_eq!(spans(&ed), vec![(0, 2..6)]);
        ed.drag_to(pos(0, 0));
        assert_eq!(spans(&ed), vec![(0, 0..2)]);
        assert_eq!(ed.primary().head, pos(0, 0));
    }

    #[test]
    fn a_double_click_selects_the_word_under_the_pointer() {
        let mut ed = stacked(&["IMG_0001 final.jpg", "b"]);
        ed.double_click(pos(0, 11));
        assert_eq!(spans(&ed), vec![(0, 9..14)], "one caret, on `final`");
        ed.double_click(pos(0, 8));
        assert_eq!(
            spans(&ed),
            vec![(0, 9..14)],
            "a space takes the word to its right"
        );
        ed.double_click(pos(0, 2));
        assert_eq!(spans(&ed), vec![(0, 0..8)], "`_` is part of a word");
        ed.double_click(pos(0, 99));
        assert_eq!(
            spans(&ed),
            vec![(0, 15..18)],
            "past the end is the last word"
        );
        // A selection made by the pointer is replaced by typing like any other.
        run(&mut ed, "png");
        assert_eq!(text(&ed), vec!["IMG_0001 final.png", "b"]);
    }

    // ── The unbound verbs ───────────────────────────────────────────────────

    #[test]
    fn select_line_selects_every_carets_whole_line() {
        let mut ed = card(&["abc", "de", "f"]);
        run(&mut ed, "<ctrl+shift+down><ctrl+shift+down>");
        assert_eq!(ed.apply(EditorOp::SelectLine), EditorEvent::Consumed);
        assert_eq!(spans(&ed), vec![(0, 0..3), (1, 0..2), (2, 0..1)]);
        run(&mut ed, "x");
        assert_eq!(text(&ed), vec!["x", "x", "x"]);
    }
}
