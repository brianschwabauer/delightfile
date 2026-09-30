//! The bulk-rename card: PLAN §5's "in-app two-column editable diff (old →
//! new) with live validation, instead of shelling to an editor", grown a
//! template field and a column that edits like a text editor.
//!
//! ## Why not `$EDITOR`
//!
//! yazi (and vidir, and every `bulk-rename.sh` on the internet) writes the names
//! to a temporary file, opens `$EDITOR` on it, and reads the file back. That is
//! a fine mechanism and a bad experience: the editor knows nothing about the
//! files, so it cannot tell you that two of your new names are the same, that
//! one of them already exists on disk, or that one of them has a `/` in it —
//! you find out afterwards, from a pile of error messages, with half the rename
//! already done.
//!
//! So the card validates *as you type*, `Enter` is refused while anything is
//! wrong, and the whole thing commits as one journal entry
//! ([`OpRecord::Renames`](df_core::ops::journal::OpRecord::Renames)) so one `u`
//! takes it all back. The opener path to `bulk-rename.txt` stays in the config
//! for anyone who wants the old way.
//!
//! ## Two ways to say what the names should be
//!
//! The field at the top holds a template, `{name}{ext}` to begin with, which is
//! every name as it is ([`template::DEFAULT`]). Each keystroke there re-resolves
//! it for every row and writes the results into the right column. That is the
//! bulk of the work: `{date}-{name}{ext}`, `holiday-{nnn}{ext}`.
//!
//! The right column is the exceptions. It is one [`NamesEditor`] over every new
//! name, with Zed's carets, so a fix to four names is four clicks with `Alt` held
//! and one word typed. A template edit **rewrites every row**, hand edits
//! included. Merging a new template into rows a person has already changed by
//! hand has no answer anyone could predict, and a card that sometimes kept an
//! edit and sometimes did not would be a card nobody could trust. That holds
//! for a row the template cannot fill as well: it shows what the file is called
//! now and says why, rather than keeping a hand edit that the wipe spared only
//! because this one template failed on it.
//!
//! What makes the wipe safe is that it is one undo step in the rows, and what
//! `Ctrl+z` in the column brings back is the rows somebody had typed into. The
//! rows nobody typed into keep following the template, because the field still
//! says it: a row that follows the template shows what the field says, so the
//! field is where it changes (`Ctrl+z` in the field undoes the field, which is
//! the same as retyping it). One step for the whole of a spell of typing in the
//! field, not one per keystroke: while nothing has been done in the rows since
//! the last rewrite, the next one takes that one back before it writes (see
//! `rewrite_on_top`), so a template typed a letter at a time is still one
//! `Ctrl+z` away from the hand edits it wiped.
//!
//! A row is **untouched** while nobody has changed its text since the template
//! last wrote the column. It then holds what the template made of it: the
//! resolved name, which is [`Bulk::derived`]'s `Ok`, or the file's old name
//! where the template could not resolve. Untouched rows are the ones the photo
//! reader and a row move may rewrite on their own; a row somebody typed into
//! is theirs.
//!
//! The editor keeps the flag
//! ([`NamesEditor::touched`](df_core::rename::editor::NamesEditor::touched)),
//! in its undo history, so `Ctrl+z` puts back whose line a row was along with
//! the line. The card used to decide instead by comparing each row with the
//! line it had last written there. That record had no history, so an undo
//! could restore a line the card had since rewritten, and the row then looked
//! typed-in with nobody having typed in it. After an undo or a redo in the
//! rows, the untouched rows are resolved again. The line put back may have
//! been written under an older template or before a photo arrived, and an
//! untouched row shows the template in the field applied to what the card
//! knows now.
//!
//! ## Photos arrive late
//!
//! `{date}`, `{taken}`, `{camera}` and the pixel sizes need the photo's EXIF,
//! and reading a hundred JPEGs off an SD card is a real read. So the card opens
//! on the stat alone ([`Facts::stat`]), and a worker thread reads the photos
//! behind it. A value that needs a photo not yet read answers
//! [`Missing::Pending`] rather than guessing, the row shows the file's old name
//! and says "reading photo…", and `Enter` waits. As each photo lands
//! ([`Bulk::poll`]) the untouched rows are resolved again.
//!
//! That second resolution is not an undo step, and it does not interrupt a word
//! being typed in another row
//! ([`NamesEditor::set_lines_quietly`](df_core::rename::editor::NamesEditor::set_lines_quietly)).
//! Nobody did it, so `Ctrl+z` is not spent on it, and `Ctrl+z` never takes back
//! only the photo. It takes back the last thing a person did. Then the rows
//! nobody typed into are filled in again from what the card knows now, photo
//! included.
//!
//! ## Files from several folders
//!
//! A search's hits (PLAN §7.2) are a selection that spans a tree, and the card
//! takes them as they are ([`Bulk::across`]). Each file is renamed in its own
//! folder and never moved, the old-name column shows the path from the root
//! so two `mod.rs` rows can be told apart, and "duplicate" and "taken" are
//! judged per folder: two `mod.rs` rows in two folders may both become
//! `lib.rs`. What else is in each folder is read from the disk when the card
//! opens, because the hits are not the folders' whole contents.
//!
//! ## The five ways a name can be wrong
//!
//! They are different problems and they read differently, so they are variants
//! and not one "invalid":
//!
//! - **Unusable**: empty, `.`/`..`, or containing a `/`. Nothing can be named
//!   this; the row is wrong on its own terms.
//! - **TooLong**: over [`NAME_MAX`] bytes.
//! - **Duplicate**: two rows in *this card* want the same name. Neither row is
//!   wrong by itself, which is why both are marked: the fix is a choice between
//!   them.
//! - **Taken**: something already on disk has that name, and it is not one of
//!   the files being renamed. A file that is being renamed *away* does not
//!   count, which is what makes swapping two names (`a`→`b`, `b`→`a`) legal
//!   here even though doing it by hand needs a temporary.
//! - **Missing**: the template asked this file for something it does not have
//!   (a PNG's taken date), or not yet ([`Problem::Pending`]). Only on an
//!   untouched row: a row typed by hand has said what it wants to be called.
//!
//! Braces in a row are not a problem. `{` and `}` are legal in a filename, and a
//! row that says `{taken}` because the PNG had no taken date is a name like any
//! other; the card says why on the row as long as the template put it there.
//!
//! The swap case is also why the rename runs in two passes; see
//! [`ordered_renames`].

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use crossbeam_channel::{unbounded, Receiver, TryRecvError};
use df_core::fs::Notifier;
use df_core::input::{is_word_char, segment_at, InputBuffer, InputEvent, InputOp};
use df_core::keymap::{Chord, Key, Mods};
use df_core::rename::complete::{self, Candidate};
use df_core::rename::editor::{EditorEvent, EditorOp, NamesEditor, Pos};
use df_core::rename::exif;
use df_core::rename::facts::{Civil, Facts, Photo, PhotoFacts};
use df_core::rename::template::{self, Missing, Part, Template};

/// The longest name this card will accept, in bytes.
///
/// Linux's own `NAME_MAX`. Refusing at 255 with a readable message beats letting
/// the rename fail with `ENAMETOOLONG` half way through a batch.
pub const NAME_MAX: usize = 255;

/// How many candidates the `{` popover shows before it scrolls. Eight is the
/// whole first stage of the catalogue's most-wanted end, and short enough that
/// the popover stays a hint under the caret rather than a second list.
pub const POPOVER_ROWS: usize = 8;

/// Rows kept between the caret and the list's edge, as the panes keep them.
const SCROLLOFF: usize = 2;

/// How many rows the list is assumed to show until the card has been measured
/// ([`Bulk::settle_layout`]). The old card's fixed height, which is about what a
/// normal window gives it.
const FIRST_GUESS_VISIBLE: usize = 12;

/// The extensions worth opening for EXIF: the JPEG family and the TIFF-built
/// raws [`exif::read`] understands. Anything else is not read at all, so a
/// folder of PDFs costs no reads and no worker.
const PHOTO_EXTENSIONS: [&str; 13] = [
    "jpg", "jpeg", "jpe", "tif", "tiff", "nef", "cr2", "dng", "arw", "orf", "rw2", "pef", "srw",
];

/// What is wrong with one row, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// Empty, `.`, `..`, or containing a `/`.
    Unusable,
    /// Another row in this card wants the same name.
    Duplicate,
    /// Something already on disk is called that.
    Taken,
    /// Over [`NAME_MAX`] bytes.
    TooLong,
    /// The template needs something this file does not have, in the words of
    /// [`Missing::Because`]: "no date taken", "no camera".
    Missing(&'static str),
    /// The template needs this file's photo facts and the reader has not got
    /// to it yet. Not an error, a wait: `Enter` holds until it clears.
    Pending,
}

impl Problem {
    /// The inline message, in the words a person can act on.
    pub fn message(self) -> &'static str {
        match self {
            Problem::Unusable => "not a usable name",
            Problem::Duplicate => "two rows want this name",
            Problem::Taken => "already exists here",
            Problem::TooLong => "too long",
            Problem::Missing(why) => why,
            Problem::Pending => "reading photo…",
        }
    }
}

/// Which of the card's two fields has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The template at the top.
    Template,
    /// The right column's editor.
    Rows,
}

/// The `{` completion list, while it is up.
#[derive(Debug, Clone, PartialEq)]
pub struct Popover {
    /// The row whose `{` opened it, or `None` for the template field.
    pub anchor_row: Option<usize>,
    /// The char index of that `{` in its line.
    pub open_at: usize,
    /// What [`complete::candidates`] offers for the text typed after the `{`.
    pub candidates: Vec<Candidate>,
    /// The highlighted candidate, an index into `candidates`.
    pub selected: usize,
    /// The first candidate drawn: the list shows [`POPOVER_ROWS`] and scrolls
    /// the selection into view.
    pub first: usize,
}

/// What a keystroke asks of whoever owns the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Handled inside the card. Repaint.
    Consumed,
    /// Nothing on the card uses this chord.
    Ignored,
    /// Carry the renames out ([`Bulk::renames`]).
    Submit,
    /// Close the card.
    Close,
    /// `Ctrl+c`, or `Ctrl+x` whose delete the card has already made: put this
    /// text on the clipboard. The card cannot do that itself, because the
    /// clipboard belongs to the window.
    Copy(String),
}

/// The photo worker's end of the channel. Dropping it is how the worker is
/// told to stop: its next send fails and it returns.
struct PhotoReader {
    replies: Receiver<(usize, Option<PhotoFacts>)>,
}

impl PhotoReader {
    /// Read `jobs` (a row's id and its file) in order on a worker, ringing
    /// `notify` after each. `None` if the thread could not be started, in which
    /// case the caller must not leave anything waiting on it.
    fn start(jobs: Vec<(usize, PathBuf)>, notify: Notifier) -> Option<PhotoReader> {
        let (tx, rx) = unbounded();
        let spawned = std::thread::Builder::new()
            .name("df-exif".to_string())
            .spawn(move || {
                df_core::thread::lower_priority(df_core::thread::NICE_INTERACTIVE);
                for (id, path) in jobs {
                    let facts = exif::read(&path);
                    if tx.send((id, facts)).is_err() {
                        // The card is gone; so is anybody to tell.
                        return;
                    }
                    notify();
                }
            });
        match spawned {
            Ok(_) => Some(PhotoReader { replies: rx }),
            Err(e) => {
                log::warn!("the photo reader did not start: {e}");
                None
            }
        }
    }
}

/// The card, while it is up (or kept as a draft, see `App::bulk_draft`).
pub struct Bulk {
    /// The listing the selection was made in. For a folder, the directory
    /// every one of these names is in. For a search's hits (PLAN §7.2) the
    /// root they are named from, while each row's file stays in its own
    /// folder ([`Bulk::folders`]).
    pub dir: PathBuf,
    /// The folder each row's file is in, in row order and swapped with the
    /// rows. Every one of them is [`Bulk::dir`] in a folder's card.
    ///
    /// **A rename never moves.** Each file is renamed inside its own folder —
    /// a name with a `/` in it is refused ([`Problem::Unusable`]) — so a card
    /// over hits from twelve folders is twelve folders' worth of renames, and
    /// never a move with a text field for a destination. Collisions are
    /// judged per folder: two `mod.rs` rows in different folders may both
    /// become `lib.rs`.
    pub folders: Vec<PathBuf>,
    /// Old names, in row order: parallel to the editor's lines and to
    /// [`Bulk::facts`] and [`Bulk::derived`], and swapped with them when a row
    /// moves.
    pub olds: Vec<String>,
    pub facts: Vec<Facts>,
    /// What the template made of each row, or why it could not.
    pub derived: Vec<Result<String, Missing>>,
    pub template: InputBuffer,
    /// The template's text, parsed. Every row resolves against it, and the
    /// field paints its values and its mistakes from it, so it is parsed once
    /// per edit rather than once per row and again every frame. Private
    /// because it must stay in step with `template` (see [`Bulk::parsed`]).
    parsed: Template,
    pub editor: NamesEditor,
    pub focus: Focus,
    pub popover: Option<Popover>,
    /// The first row drawn.
    pub first: usize,
    /// How many rows the card has room for, as the last layout measured it.
    pub visible: usize,
    /// How far the template field's text is scrolled to keep its caret in view.
    pub template_scroll: f32,
    /// …and the primary caret's row's.
    pub row_scroll: f32,
    /// Each row's position when the card opened, swapped with the rows, so a
    /// photo the worker read for "row 7" finds that file after a row move.
    ids: Vec<usize>,
    /// Names already in each row's folder that are not part of this card: the
    /// "taken" test, snapshotted when the card opened rather than re-read per
    /// keystroke.
    others: HashMap<PathBuf, HashSet<String>>,
    photos: Option<PhotoReader>,
    /// A wheel's fractions of a row, until they add up to one.
    carry: f32,
    /// Whether the editor's newest undo step is the template's own rewrite,
    /// with nothing done in the rows since. While it is, the next rewrite
    /// replaces that step rather than stacking another on it.
    rewrite_on_top: bool,
}

impl Bulk {
    /// Open the card over a selection.
    ///
    /// `siblings` is every name in the directory; the ones being renamed are
    /// removed from it here, so a row keeping its own name is not reported as
    /// colliding with itself. `notify` rings the event loop when the photo
    /// reader has something to say.
    ///
    /// **The directory has to be one on this machine.** This card commits with
    /// [`df_core::ops::rename`] — a `rename(2)` against `dir.join(name)` — and
    /// a remote pane's `dir` is an `sftp://…` display path, which resolves
    /// against the process's own working directory. The command dispatch's
    /// remote list cannot express "only with two rows selected", so the refusal
    /// lives in the constructor: there is no way to build a card pointed at a
    /// server, and the caller has a sentence to show instead.
    pub fn new(
        dir: PathBuf,
        names: Vec<String>,
        siblings: &[String],
        notify: Notifier,
    ) -> Result<Bulk, &'static str> {
        if crate::remote::is_remote(&dir) {
            return Err("Rename one at a time over the link — r renames the row under the cursor");
        }
        let others = HashMap::from([(dir.clone(), others_of(siblings, &names))]);
        let rows = names.into_iter().map(|name| (dir.clone(), name)).collect();
        Ok(Bulk::build(dir, rows, others, notify))
    }

    /// Open the card over files from several folders: a search's hits
    /// (PLAN §7.2), whose selection spans a tree. `dir` is the root the rows
    /// are named from, and `paths` the files, in the pane's order.
    ///
    /// What else is in each folder is read here, one `read_dir` per folder the
    /// selection touches: the hits are not the folders' whole contents, and a
    /// name taken by a file the search did not return is still taken.
    pub fn across(dir: PathBuf, paths: &[PathBuf], notify: Notifier) -> Bulk {
        let rows = rows_of(paths);
        let others = others_on_disk(&rows);
        Bulk::build(dir, rows, others, notify)
    }

    /// The card over `(folder, name)` rows, with the photo reader started on
    /// the ones that could be photos.
    fn build(
        dir: PathBuf,
        rows: Vec<(PathBuf, String)>,
        others: HashMap<PathBuf, HashSet<String>>,
        notify: Notifier,
    ) -> Bulk {
        let mut facts: Vec<Facts> = rows
            .iter()
            .map(|(folder, name)| Facts::stat(folder, name))
            .collect();
        let mut jobs = Vec::new();
        for (id, row) in facts.iter_mut().enumerate() {
            if could_be_photo(row) {
                jobs.push((id, rows[id].0.join(&row.name)));
            } else {
                row.photo = Photo::None;
            }
        }
        let photos = if jobs.is_empty() {
            None
        } else {
            PhotoReader::start(jobs, notify)
        };
        if photos.is_none() {
            // No reader, so nothing may wait on one: a `Pending` with no
            // worker behind it is an `Enter` that never comes back.
            for row in &mut facts {
                if row.photo == Photo::Pending {
                    row.photo = Photo::None;
                }
            }
        }
        let folders = rows.into_iter().map(|(folder, _)| folder).collect();
        let mut bulk = Bulk::from_rows(dir, folders, facts, others);
        bulk.photos = photos;
        bulk
    }

    /// The card over one folder's facts already gathered, with no photo
    /// reader: what the tests build directly so that a photo arrives exactly
    /// when they say.
    #[cfg(test)]
    fn from_facts(dir: PathBuf, facts: Vec<Facts>, siblings: &[String]) -> Bulk {
        let names: Vec<String> = facts.iter().map(|row| row.name.clone()).collect();
        let others = HashMap::from([(dir.clone(), others_of(siblings, &names))]);
        let folders = vec![dir.clone(); facts.len()];
        Bulk::from_rows(dir, folders, facts, others)
    }

    /// The card over facts already gathered, each in its folder.
    fn from_rows(
        dir: PathBuf,
        folders: Vec<PathBuf>,
        facts: Vec<Facts>,
        others: HashMap<PathBuf, HashSet<String>>,
    ) -> Bulk {
        let olds: Vec<String> = facts.iter().map(|row| row.name.clone()).collect();
        let parsed = Template::parse(template::DEFAULT);
        let now = Civil::now();
        let derived: Vec<Result<String, Missing>> = facts
            .iter()
            .enumerate()
            .map(|(index, row)| parsed.resolve(row, index, now))
            .collect();
        let lines: Vec<String> = derived
            .iter()
            .zip(&olds)
            .map(|(name, old)| name.clone().unwrap_or_else(|_| old.clone()))
            .collect();
        let rows = olds.len();
        Bulk {
            dir,
            folders,
            editor: NamesEditor::new(lines),
            olds,
            facts,
            derived,
            template: InputBuffer::new(template::DEFAULT, template::DEFAULT.chars().count()),
            parsed,
            // The template first: it is the tool for the bulk of the work, and
            // a keystroke that landed in row 0 alone would be the one row the
            // card is least about.
            focus: Focus::Template,
            popover: None,
            first: 0,
            visible: FIRST_GUESS_VISIBLE,
            template_scroll: 0.0,
            row_scroll: 0.0,
            ids: (0..rows).collect(),
            others,
            photos: None,
            carry: 0.0,
            rewrite_on_top: false,
        }
    }

    /// Take a draft kept from an earlier card back, if it was over this same
    /// selection: the same directory and the same set of names, in whatever
    /// order. The draft's own order wins, because it may be the order somebody
    /// arranged with `Alt+↑↓`. What else is in the directory is read again, as
    /// it may have changed while the card was closed.
    pub fn reopen(
        draft: Box<Bulk>,
        dir: &Path,
        names: &[String],
        siblings: &[String],
    ) -> Option<Box<Bulk>> {
        let rows: Vec<(PathBuf, String)> = names
            .iter()
            .map(|name| (dir.to_path_buf(), name.clone()))
            .collect();
        let others = HashMap::from([(dir.to_path_buf(), others_of(siblings, names))]);
        Bulk::reopen_rows(draft, dir, &rows, others)
    }

    /// [`Bulk::reopen`] for a card over files from several folders
    /// ([`Bulk::across`]).
    pub fn reopen_across(draft: Box<Bulk>, dir: &Path, paths: &[PathBuf]) -> Option<Box<Bulk>> {
        let rows = rows_of(paths);
        let others = others_on_disk(&rows);
        Bulk::reopen_rows(draft, dir, &rows, others)
    }

    /// The same selection is the same listing and the same set of files —
    /// each name in its own folder.
    fn reopen_rows(
        draft: Box<Bulk>,
        dir: &Path,
        rows: &[(PathBuf, String)],
        others: HashMap<PathBuf, HashSet<String>>,
    ) -> Option<Box<Bulk>> {
        if draft.dir != dir || draft.olds.len() != rows.len() {
            return None;
        }
        let ours: HashSet<(&Path, &str)> = draft
            .folders
            .iter()
            .zip(&draft.olds)
            .map(|(folder, old)| (folder.as_path(), old.as_str()))
            .collect();
        if rows
            .iter()
            .any(|(folder, name)| !ours.contains(&(folder.as_path(), name.as_str())))
        {
            return None;
        }
        let mut draft = draft;
        draft.others = others;
        // The draft's own parse would do. Parsing again costs nothing and means
        // a card never comes back painting one template and resolving another.
        draft.parsed = Template::parse(draft.template.text());
        // A list left open by a click on the `×` is not what anyone comes back
        // for.
        draft.popover = None;
        Some(draft)
    }

    // ── What the card says ──────────────────────────────────────────────────

    /// How many rows the card has.
    pub fn len(&self) -> usize {
        self.olds.len()
    }

    /// The template field's text, parsed: what every row resolves against
    /// and what the field paints its values and mistakes from.
    pub fn parsed(&self) -> &Template {
        &self.parsed
    }

    /// Whether row `row` is still the template's: nobody has changed its text
    /// since the template last wrote the column (see the module notes).
    pub fn untouched(&self, row: usize) -> bool {
        !self.editor.touched(row)
    }

    /// What the old-name column says for row `row`: the name, or — in a card
    /// over several folders — the path to it from the listing's root, since
    /// three rows reading `mod.rs` would be three rows nobody could tell apart.
    pub fn label(&self, row: usize) -> String {
        let (Some(folder), Some(old)) = (self.folders.get(row), self.olds.get(row)) else {
            return String::new();
        };
        if *folder == self.dir {
            return old.clone();
        }
        crate::hits::name_under(&self.dir, &folder.join(old))
    }

    /// What is wrong with each row, in row order.
    ///
    /// Judged folder by folder: a name is a duplicate only of another row in
    /// the same folder, and taken only by a file in its own.
    pub fn problems(&self) -> Vec<Option<Problem>> {
        let lines = self.editor.lines();
        let mut found = vec![None; lines.len()];
        let none = HashSet::new();
        for (folder, rows) in self.by_folder() {
            let names: Vec<&str> = rows
                .iter()
                .filter_map(|&row| lines.get(row).map(String::as_str))
                .collect();
            let others = self.others.get(folder).unwrap_or(&none);
            for (row, problem) in rows.iter().zip(problems(&names, others)) {
                found[*row] = problem;
            }
        }
        for (row, problem) in found.iter_mut().enumerate() {
            if problem.is_some() || !self.untouched(row) {
                continue;
            }
            *problem = match self.derived.get(row) {
                Some(Err(Missing::Pending)) => Some(Problem::Pending),
                Some(Err(Missing::Because(why))) => Some(Problem::Missing(why)),
                _ => None,
            };
        }
        found
    }

    /// Whether `Enter` is allowed.
    pub fn valid(&self) -> bool {
        self.problems().iter().all(Option::is_none)
    }

    /// How many rows would actually change.
    pub fn changes(&self) -> usize {
        self.olds
            .iter()
            .zip(self.editor.lines())
            .filter(|(old, new)| old != new)
            .count()
    }

    /// The new name of the first row that changes, which is where the cursor
    /// goes once the card has done its work.
    pub fn first_change(&self) -> Option<String> {
        self.olds
            .iter()
            .zip(self.editor.lines())
            .find(|(old, new)| old != new)
            .map(|(_, new)| new.clone())
    }

    /// The file the first row that changes names, *before* the card renames
    /// it — what a card over several folders follows to land the cursor on,
    /// through the renames as they ran (a folder it is in may be renamed too).
    pub fn first_changed(&self) -> Option<PathBuf> {
        self.olds
            .iter()
            .zip(self.editor.lines())
            .zip(&self.folders)
            .find(|((old, new), _)| old != new)
            .map(|((old, _), folder)| folder.join(old))
    }

    /// The renames to carry out, in an order that is safe to run one at a time.
    ///
    /// Ordered folder by folder: a rename never crosses one, so the only
    /// collisions (and the only swaps) are between rows in the same folder.
    ///
    /// **The deepest folders first.** Over a search's hits a card can hold a
    /// folder and files inside it, and a file whose folder has already been
    /// renamed is not at the path the card knows it by any more. Renamed
    /// before its folder, it is — and then goes with the folder.
    pub fn renames(&self) -> Vec<(PathBuf, PathBuf)> {
        let lines = self.editor.lines();
        let mut out = Vec::new();
        let mut groups = self.by_folder();
        groups.sort_by_key(|(folder, _)| std::cmp::Reverse(folder.components().count()));
        for (folder, rows) in groups {
            let pairs: Vec<(String, String)> = rows
                .iter()
                .filter_map(|&row| Some((self.olds.get(row)?, lines.get(row)?)))
                .filter(|(old, new)| old != new)
                .map(|(old, new)| (old.clone(), new.clone()))
                .collect();
            out.extend(ordered_renames(folder, &pairs));
        }
        out
    }

    /// The rows, grouped by the folder they are in: each folder once, in the
    /// order it first appears, with its rows in row order. One pass, because
    /// the card asks for its problems every frame.
    fn by_folder(&self) -> Vec<(&PathBuf, Vec<usize>)> {
        let mut groups: Vec<(&PathBuf, Vec<usize>)> = Vec::new();
        let mut at: HashMap<&PathBuf, usize> = HashMap::new();
        for (row, folder) in self.folders.iter().enumerate() {
            let group = *at.entry(folder).or_insert_with(|| {
                groups.push((folder, Vec::new()));
                groups.len() - 1
            });
            groups[group].1.push(row);
        }
        groups
    }

    /// What a candidate would make of the row the popover is about (row 0 for
    /// the template field): the popover's right-hand column.
    pub fn preview(&self, candidate: &Candidate) -> Result<String, Missing> {
        let row = self
            .popover
            .as_ref()
            .and_then(|popover| popover.anchor_row)
            .unwrap_or(0);
        let Some(facts) = self.facts.get(row) else {
            return Err(Missing::Because("no file"));
        };
        Template::parse(&candidate.insert).resolve(facts, row, Civil::now())
    }

    /// The popover's candidates, if it is up and has any. A popover whose
    /// query matches nothing draws nothing and takes no keys, so a typo inside
    /// braces cannot swallow `Enter`.
    pub fn live_popover(&self) -> Option<&Popover> {
        self.popover
            .as_ref()
            .filter(|popover| !popover.candidates.is_empty())
    }

    // ── The photo reader ────────────────────────────────────────────────────

    /// Take whatever the photo reader has read since the last frame, and bring
    /// the untouched rows up to date with it in one edit. Returns whether
    /// anything changed, so the frame repaints.
    pub fn poll(&mut self) -> bool {
        let Some(reader) = &self.photos else {
            return false;
        };
        let mut arrived = Vec::new();
        let mut gone = false;
        loop {
            match reader.replies.try_recv() {
                Ok(reply) => arrived.push(reply),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    gone = true;
                    break;
                }
            }
        }
        if gone {
            // The worker has finished, or died. Either way nothing more is
            // coming, and a row still waiting would wait for ever.
            self.photos = None;
            for row in &mut self.facts {
                if row.photo == Photo::Pending {
                    row.photo = Photo::None;
                }
            }
        }
        if arrived.is_empty() && !gone {
            return false;
        }
        for (id, photo) in arrived {
            self.photo_arrived(id, photo);
        }
        self.refresh();
        true
    }

    /// One photo's facts, for the row that was `id` when the card opened.
    fn photo_arrived(&mut self, id: usize, photo: Option<PhotoFacts>) {
        if let Some(row) = self.ids.iter().position(|&i| i == id) {
            self.facts[row].photo = photo.map_or(Photo::None, Photo::Some);
        }
    }

    // ── Resolving ───────────────────────────────────────────────────────────

    /// The template changed: parse it again, resolve it for every row and
    /// rewrite the column, hand edits and all, as one undo step.
    ///
    /// A row the template cannot resolve shows the file's old name and says
    /// why. It does not keep the line it had. A template edit wipes the
    /// column, and a row it cannot fill starts from what the file is called
    /// now. A hand edit that happened to be on that row is one `Ctrl+z` away,
    /// like the hand edits on every other row. Keeping it would make the wipe
    /// depend on whether this template happened to fail on that row.
    fn rewrite(&mut self) {
        if self.rewrite_on_top {
            // The rows still show the last rewrite and nothing else, so it is
            // taken back first: a spell of typing in the field is one step of
            // the rows' undo, not one per letter.
            self.editor.apply(EditorOp::Undo);
        }
        self.parsed = Template::parse(self.template.text());
        let now = Civil::now();
        let before = self.editor.lines().to_vec();
        let mut lines = Vec::with_capacity(self.facts.len());
        for (row, facts) in self.facts.iter().enumerate() {
            let result = self.parsed.resolve(facts, row, now);
            let line = match &result {
                Ok(name) => name.clone(),
                Err(_) => self.olds[row].clone(),
            };
            self.derived[row] = result;
            lines.push(line);
        }
        self.editor.set_lines(lines);
        // Identical lines push no step, and then there is nothing on top to
        // take back next time.
        self.rewrite_on_top = self.editor.lines() != before.as_slice();
    }

    /// Resolve the template again for the untouched rows only: a photo has
    /// arrived, rows have moved and the counter numbers by position, or an
    /// undo or a redo has put back lines written against other facts or
    /// another template. Rows somebody has typed into are left as they are.
    ///
    /// Not an undo step, and not the end of a word being typed. The template
    /// has not changed; the facts under it have, or the rows' order. Nobody
    /// asked for that with a keystroke, so a keystroke must not be spent on
    /// taking it back (see
    /// [`NamesEditor::set_lines_quietly`](df_core::rename::editor::NamesEditor::set_lines_quietly)).
    fn refresh(&mut self) {
        // Nothing has been done in the rows since the template wrote them, so
        // every row is untouched and a refresh is a rewrite, which folds into
        // the step already there.
        if self.rewrite_on_top {
            self.rewrite();
            return;
        }
        let now = Civil::now();
        let mut lines = self.editor.lines().to_vec();
        for (row, line) in lines.iter_mut().enumerate() {
            if !self.untouched(row) {
                continue;
            }
            let result = self.parsed.resolve(&self.facts[row], row, now);
            if let Ok(name) = &result {
                line.clone_from(name);
            }
            self.derived[row] = result;
        }
        self.editor.set_lines_quietly(lines);
    }

    /// `}` was typed (or a completion accepted, or text pasted) in the rows:
    /// every caret that now sits just after a whole `{…}` value has that value
    /// resolved for its own row, as one edit. A value that cannot resolve for
    /// that row stays as the literal text it is; a value never outlives the
    /// keystroke that closed it.
    fn expand_tokens(&mut self) {
        let now = Civil::now();
        let mut edits: Vec<(usize, Range<usize>, String)> = Vec::new();
        for cursor in self.editor.cursors() {
            if cursor.is_selection() {
                continue;
            }
            let Pos { row, col } = cursor.head;
            let chars: Vec<char> = self.editor.line(row).unwrap_or("").chars().collect();
            let Some(open) = open_brace(&chars, col) else {
                continue;
            };
            let slice: String = chars[open..col].iter().collect();
            let (Some(token), Some(facts)) = (single_value(&slice), self.facts.get(row)) else {
                continue;
            };
            let Ok(name) = token.resolve(facts, row, now) else {
                continue;
            };
            // Ranges on one row must not overlap; carets are sorted, so the
            // only one this could overlap is the last one taken.
            if edits
                .last()
                .is_some_and(|(last, range, _)| *last == row && range.end > open)
            {
                continue;
            }
            edits.push((row, open..col, name));
        }
        if !edits.is_empty() {
            self.editor.replace_ranges(edits);
        }
    }

    /// Follow every row move the editor has made since the last look: the old
    /// names, the facts and what the template made of each row travel with
    /// their line. Answers whether anything moved, because then the untouched
    /// rows have to be resolved again: the counter numbers rows by position,
    /// and a moved row has a new one.
    fn mirror_swaps(&mut self) -> bool {
        let mut moved = false;
        while let Some((a, b)) = self.editor.take_swap() {
            if a.max(b) >= self.len() {
                continue;
            }
            self.olds.swap(a, b);
            self.folders.swap(a, b);
            self.facts.swap(a, b);
            self.derived.swap(a, b);
            self.ids.swap(a, b);
            moved = true;
        }
        moved
    }

    // ── Keys ────────────────────────────────────────────────────────────────

    /// One keystroke, to the popover if it is up, else to whichever field has
    /// the keyboard.
    pub fn key(&mut self, chord: Chord) -> Outcome {
        if let Some(outcome) = self.popover_key(chord) {
            return outcome;
        }
        match self.focus {
            Focus::Template => self.template_key(chord),
            Focus::Rows => self.rows_key(chord),
        }
    }

    /// The popover's own keys: move, accept, dismiss. Everything else goes on
    /// to the field and narrows the list.
    fn popover_key(&mut self, chord: Chord) -> Option<Outcome> {
        let count = self.live_popover()?.candidates.len();
        if !chord.mods.is_none() {
            return None;
        }
        match chord.key {
            Key::ArrowUp => self.move_selection(count - 1, count),
            Key::ArrowDown => self.move_selection(1, count),
            Key::Tab | Key::Enter => self.accept(),
            Key::Escape => self.popover = None,
            _ => return None,
        }
        Some(Outcome::Consumed)
    }

    /// Move the highlight by `step` (mod `count`, so `count - 1` is one up),
    /// wrapping, and scroll it into view.
    fn move_selection(&mut self, step: usize, count: usize) {
        if let Some(popover) = &mut self.popover {
            popover.selected = (popover.selected + step) % count.max(1);
            if popover.selected < popover.first {
                popover.first = popover.selected;
            } else if popover.selected >= popover.first + POPOVER_ROWS {
                popover.first = popover.selected + 1 - POPOVER_ROWS;
            }
        }
    }

    fn template_key(&mut self, chord: Chord) -> Outcome {
        let plain = chord.mods.is_none();
        match chord.key {
            Key::Tab if plain || chord.mods == Mods::SHIFT => {
                self.focus_rows();
                return Outcome::Consumed;
            }
            Key::ArrowDown if plain => {
                self.focus_rows();
                return Outcome::Consumed;
            }
            Key::Enter if plain => return self.enter(false),
            Key::Escape if plain => return Outcome::Close,
            // The field's own map reads `Ctrl+c` as cancel, which on this card
            // would close it on a person reaching for copy. Without a
            // selection there is nothing to copy, and the press does nothing.
            Key::Char('c') if chord.mods == Mods::CTRL => {
                return match self.template_selection() {
                    Some(text) => Outcome::Copy(text),
                    None => Outcome::Consumed,
                };
            }
            Key::Char('x') if chord.mods == Mods::CTRL => {
                let Some(text) = self.template_selection() else {
                    return Outcome::Consumed;
                };
                // A delete with a selection takes the selection, as one step
                // of the field's undo.
                self.template.apply(InputOp::Backspace);
                self.rewrite();
                self.sync_popover(false);
                return Outcome::Copy(text);
            }
            _ => {}
        }
        let before = self.template.text().to_string();
        match self.template.feed(chord) {
            InputEvent::Consumed => {
                // Only a change to the text rewrites the rows: a caret moving
                // along the template must not throw a hand edit away.
                if self.template.text() != before {
                    self.rewrite();
                }
                self.sync_popover(typed(chord) == Some('{'));
                Outcome::Consumed
            }
            InputEvent::Submit(_) => self.enter(false),
            InputEvent::Cancel => Outcome::Close,
        }
    }

    /// The template field's selected text, if it has a selection.
    fn template_selection(&self) -> Option<String> {
        let range = self.template.selection_bytes()?;
        self.template.text().get(range).map(str::to_string)
    }

    fn rows_key(&mut self, chord: Chord) -> Outcome {
        let plain = chord.mods.is_none();
        let ctrl = chord.mods == Mods::CTRL;
        match chord.key {
            Key::Tab if plain || chord.mods == Mods::SHIFT => {
                self.focus_template();
                return Outcome::Consumed;
            }
            // Up off the top row is up into the field above it, as it is in
            // every form. Only with one caret: a stack of carets pressing up
            // is moving the stack. And only with nothing selected: `↑` drops
            // a selection first, as it does on every other row, so the press
            // that leaves the rows is the next one.
            Key::ArrowUp
                if plain
                    && self.editor.cursors().len() == 1
                    && self.editor.primary().head.row == 0
                    && !self.editor.primary().is_selection() =>
            {
                self.focus_template();
                return Outcome::Consumed;
            }
            Key::ArrowUp if ctrl => {
                self.jump_to_problem(false);
                return Outcome::Consumed;
            }
            Key::ArrowDown if ctrl => {
                self.jump_to_problem(true);
                return Outcome::Consumed;
            }
            Key::Enter if plain => return self.enter(true),
            // Copy and cut are the card's, not the editor's: the editor does
            // not own the clipboard (see its module header). With nothing
            // selected they do nothing. They are never `Esc`, because a
            // `Ctrl+c` meant as copy must not throw the card away.
            Key::Char('c') if ctrl => {
                let text = self.editor.selected_text();
                return if text.is_empty() {
                    Outcome::Consumed
                } else {
                    Outcome::Copy(text)
                };
            }
            Key::Char('x') if ctrl => {
                let text = self.editor.selected_text();
                if text.is_empty() {
                    return Outcome::Consumed;
                }
                self.rewrite_on_top = false;
                self.editor.delete_selections();
                self.follow_primary();
                self.sync_popover(false);
                return Outcome::Copy(text);
            }
            _ => {}
        }
        self.rewrite_on_top = false;
        match self.editor.feed(chord) {
            EditorEvent::Consumed => {
                let typed = typed(chord);
                if typed == Some('}') {
                    self.expand_tokens();
                }
                // A row move renumbers the rows. An undo or a redo can put back
                // lines from before a photo arrived, on rows it has just made
                // untouched again. Either way the untouched rows are resolved
                // against what the card knows now.
                let moved = self.mirror_swaps();
                if moved || self.editor.last_was_history() {
                    self.refresh();
                }
                self.follow_primary();
                self.sync_popover(typed == Some('{'));
                Outcome::Consumed
            }
            EditorEvent::Submit => self.enter(true),
            EditorEvent::Cancel => Outcome::Close,
            EditorEvent::Ignored => Outcome::Ignored,
        }
    }

    /// Composed text (an IME commit, a dead-key sequence) or a paste, into
    /// whichever field has the keyboard.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.focus {
            Focus::Template => {
                let before = self.template.text().to_string();
                self.template.insert_text(text);
                if self.template.text() != before {
                    self.rewrite();
                }
            }
            Focus::Rows => {
                self.rewrite_on_top = false;
                self.editor.insert_text(text);
                if text.contains('}') {
                    self.expand_tokens();
                }
                self.follow_primary();
            }
        }
        self.sync_popover(text.ends_with('{'));
    }

    /// `Enter`. A card with a settled problem refuses, and from the rows the
    /// caret goes to the first row that has one (the status beside the
    /// buttons has already said why). A card waiting only on the photo reader refuses without moving
    /// anything: the wait ends on its own. A card with nothing changed closes,
    /// and anything else is submitted.
    fn enter(&mut self, jump: bool) -> Outcome {
        let problems = self.problems();
        let settled = problems
            .iter()
            .position(|problem| problem.is_some_and(|p| p != Problem::Pending));
        if let Some(row) = settled {
            if jump {
                self.rewrite_on_top = false;
                self.editor.go_to_row(row);
                self.follow_primary();
                self.popover = None;
            }
            return Outcome::Consumed;
        }
        if problems.iter().any(Option::is_some) {
            return Outcome::Consumed;
        }
        if self.changes() == 0 {
            return Outcome::Close;
        }
        Outcome::Submit
    }

    /// `Ctrl+↑` / `Ctrl+↓`: one caret on the previous or next row that has a
    /// problem, wrapping. Nothing to jump to is nothing done.
    ///
    /// A row waiting on the photo reader is not a problem to go to, as it is
    /// not one for `Enter` to go to: there is nothing to fix there, and the
    /// wait ends on its own.
    fn jump_to_problem(&mut self, forward: bool) {
        let rows: Vec<usize> = self
            .problems()
            .iter()
            .enumerate()
            .filter_map(|(row, problem)| {
                problem
                    .filter(|&problem| problem != Problem::Pending)
                    .map(|_| row)
            })
            .collect();
        let here = self.editor.primary().head.row;
        let target = if forward {
            rows.iter().find(|&&row| row > here).or(rows.first())
        } else {
            rows.iter().rev().find(|&&row| row < here).or(rows.last())
        };
        if let Some(&row) = target {
            self.rewrite_on_top = false;
            self.editor.go_to_row(row);
            self.follow_primary();
            self.sync_popover(false);
        }
    }

    fn focus_template(&mut self) {
        self.focus = Focus::Template;
        self.popover = None;
    }

    /// Into the rows. The carets stay where they were left, unless none of
    /// them is on screen, in which case there is one on the first row shown:
    /// the keyboard must land somewhere the eye can see.
    fn focus_rows(&mut self) {
        self.focus = Focus::Rows;
        self.popover = None;
        let shown = self.first..self.first + self.visible.max(1);
        if !self
            .editor
            .cursors()
            .iter()
            .any(|cursor| shown.contains(&cursor.head.row))
        {
            self.rewrite_on_top = false;
            self.editor.go_to_row(self.first);
        }
    }

    /// Keep the primary caret's row on screen, by the panes' scrolloff rule.
    fn follow_primary(&mut self) {
        let row = self.editor.primary().head.row;
        self.first =
            crate::viewport::first_visible(self.first, row, self.len(), self.visible, SCROLLOFF);
    }

    // ── The popover ─────────────────────────────────────────────────────────

    /// The focused field's primary caret: which row (`None` for the template),
    /// where, and the line it is on.
    fn caret(&self) -> (Option<usize>, usize, Vec<char>) {
        match self.focus {
            Focus::Template => (
                None,
                self.template.cursor(),
                self.template.text().chars().collect(),
            ),
            Focus::Rows => {
                let head = self.editor.primary().head;
                let line = self.editor.line(head.row).unwrap_or("");
                (Some(head.row), head.col, line.chars().collect())
            }
        }
    }

    /// Bring the popover in line with the caret after anything that may have
    /// moved it. An open popover stays while the caret is still after its `{`
    /// with no brace typed since, and re-filters on what has been typed; any
    /// other caret closes it. `opened` is whether the edit just made typed a
    /// `{`, which is the only thing that opens one: arrowing past a `{` that
    /// was already there is not asking for a list.
    ///
    /// The `{` just typed opens the list only if it opens a value, by the same
    /// rule [`open_brace`] reads a value back with. `{{` is a literal brace
    /// and asks for nothing, and `{{{` is a literal brace followed by an
    /// opener, which does.
    fn sync_popover(&mut self, opened: bool) {
        let (row, caret, chars) = self.caret();
        let rows = self.len();
        if let Some(popover) = &mut self.popover {
            let typed = (popover.anchor_row == row)
                .then(|| token_typed(&chars, popover.open_at, caret))
                .flatten();
            if let Some(typed) = typed {
                let candidates = complete::candidates(&typed, rows);
                if candidates != popover.candidates {
                    popover.candidates = candidates;
                    popover.selected = 0;
                    popover.first = 0;
                }
                return;
            }
            self.popover = None;
        }
        let opens = caret >= 1 && opens_value(&chars, caret - 1);
        if opened && opens {
            self.popover = Some(Popover {
                anchor_row: row,
                open_at: caret - 1,
                candidates: complete::candidates("", rows),
                selected: 0,
                first: 0,
            });
        }
    }

    /// Put the highlighted candidate in place of the `{` and what was typed
    /// after it.
    ///
    /// In the template it goes in as written, placeholders and all: the field
    /// is where values live. In the rows a value never stays a value, so each
    /// caret that has the same `{…` before it gets the candidate resolved for
    /// its own row, in one edit; a row it cannot resolve for keeps the literal
    /// text.
    fn accept(&mut self) {
        let Some(popover) = self.popover.take() else {
            return;
        };
        let Some(candidate) = popover.candidates.get(popover.selected) else {
            return;
        };
        let (_, caret, chars) = self.caret();
        if caret <= popover.open_at {
            return;
        }
        match popover.anchor_row {
            None => {
                self.template.set_selection(popover.open_at, caret);
                self.template.insert_text(&candidate.insert);
                self.rewrite();
            }
            Some(_) => {
                let typed = &chars[popover.open_at..caret];
                let value = single_value(&candidate.insert);
                let now = Civil::now();
                let mut edits: Vec<(usize, Range<usize>, String)> = Vec::new();
                for cursor in self.editor.cursors() {
                    if cursor.is_selection() {
                        continue;
                    }
                    let Pos { row, col } = cursor.head;
                    let line: Vec<char> = self.editor.line(row).unwrap_or("").chars().collect();
                    let Some(start) = col.checked_sub(typed.len()) else {
                        continue;
                    };
                    if line.get(start..col) != Some(typed) {
                        continue;
                    }
                    if edits
                        .last()
                        .is_some_and(|(last, range, _)| *last == row && range.end > start)
                    {
                        continue;
                    }
                    let name = value
                        .as_ref()
                        .zip(self.facts.get(row))
                        .and_then(|(value, facts)| value.resolve(facts, row, now).ok())
                        .unwrap_or_else(|| candidate.insert.clone());
                    edits.push((row, start..col, name));
                }
                self.rewrite_on_top = false;
                self.editor.replace_ranges(edits);
                self.follow_primary();
            }
        }
    }

    // ── The pointer ─────────────────────────────────────────────────────────

    /// A press in the template field: `at` is the boundary nearest the
    /// pointer, `under` the character it is on, `clicks` how many quick
    /// presses the run has made (a caret, a word, the line).
    pub fn click_template(&mut self, at: usize, under: usize, clicks: u8, shift: bool) {
        self.focus = Focus::Template;
        let text = self.template.text();
        match clicks {
            2 => {
                let run = segment_at(text, under, |c| !is_word_char(c));
                self.template.set_selection(run.start, run.end);
            }
            3.. => {
                let len = text.chars().count();
                self.template.set_selection(0, len);
            }
            _ => self.template.move_to(at, shift),
        }
        self.sync_popover(false);
    }

    /// A drag from a single press in the template field has reached `at`.
    pub fn drag_template(&mut self, at: usize) {
        self.template.move_to(at, true);
        self.sync_popover(false);
    }

    /// A press on a row's new name. `at` is the boundary nearest the pointer
    /// and `under` the character it is on. `Alt` adds a caret, `Shift` grows
    /// the primary's selection; two presses take a word and three the line.
    pub fn click_row(&mut self, at: Pos, under: usize, clicks: u8, shift: bool, alt: bool) {
        self.focus = Focus::Rows;
        self.rewrite_on_top = false;
        match clicks {
            2 => self.editor.double_click(Pos {
                row: at.row,
                col: under,
            }),
            3.. => {
                self.editor.click(at, false, false);
                self.editor.apply(EditorOp::SelectLine);
            }
            _ => self.editor.click(at, shift, alt),
        }
        self.sync_popover(false);
    }

    /// A drag from a single press on a row has reached column `col` of the
    /// row it started on.
    pub fn drag_row(&mut self, col: usize) {
        self.rewrite_on_top = false;
        let row = self.editor.primary().anchor.row;
        self.editor.drag_to(Pos { row, col });
        self.sync_popover(false);
    }

    /// A click on candidate `index`: the same as arrowing to it and `Tab`.
    pub fn pick(&mut self, index: usize) {
        if let Some(popover) = &mut self.popover {
            if index < popover.candidates.len() {
                popover.selected = index;
            }
        }
        self.accept();
    }

    /// A wheel over the list, in rows (fractional; positive is down). Whole
    /// rows at a time, with the fractions carried, as the tray scrolls.
    pub fn wheel(&mut self, rows: f32) {
        self.carry += rows;
        let whole = self.carry.trunc();
        self.carry -= whole;
        let last = self.len().saturating_sub(self.visible) as i64;
        let moved = (self.first as i64 + whole as i64).clamp(0, last);
        if (moved == 0 && self.carry < 0.0) || (moved == last && self.carry > 0.0) {
            self.carry = 0.0;
        }
        self.first = moved as usize;
    }

    /// Show the list from row `first` (the scrollbar's thumb, or its track).
    pub fn scroll_to(&mut self, first: usize) {
        self.first = first.min(self.len().saturating_sub(self.visible));
    }

    /// What the layout measured this frame: how many rows fit, and how far the
    /// two lines with a caret in them are scrolled. `PgUp`/`PgDn` move a card's
    /// worth, and a card that has grown keeps no gap under its last row.
    pub fn settle_layout(&mut self, visible: usize, template_scroll: f32, row_scroll: f32) {
        self.visible = visible.max(1);
        self.editor.set_page(self.visible);
        self.first = self.first.min(self.len().saturating_sub(self.visible));
        self.template_scroll = template_scroll;
        self.row_scroll = row_scroll;
    }
}

/// Whether a row is worth handing to the photo reader: a file (not a folder)
/// whose extension is one [`exif::read`] could have something to say about.
fn could_be_photo(facts: &Facts) -> bool {
    if facts.is_dir {
        return false;
    }
    let ext = facts.ext().trim_start_matches('.').to_ascii_lowercase();
    PHOTO_EXTENSIONS.contains(&ext.as_str())
}

/// The character a chord types, if it types one: the editors' rule, where a
/// key is stored unshifted and Shift is applied here.
fn typed(chord: Chord) -> Option<char> {
    let m = chord.mods;
    if m.ctrl || m.alt || m.super_key {
        return None;
    }
    match chord.key {
        Key::Space => Some(' '),
        Key::Char(c) if m.shift => chord.key.shifted_glyph().or(Some(c)),
        Key::Char(c) => Some(c),
        _ => None,
    }
}

/// What has been typed after the `{` at `open_at`, while the caret is still
/// inside that value: after the `{`, with no brace in between.
fn token_typed(chars: &[char], open_at: usize, caret: usize) -> Option<String> {
    if caret <= open_at || chars.get(open_at) != Some(&'{') {
        return None;
    }
    let typed: String = chars.get(open_at + 1..caret)?.iter().collect();
    (!typed.contains(['{', '}'])).then_some(typed)
}

/// Where the value that ends just before `caret` opens: the nearest `{` back
/// along the line with no `}` between, when the character before the caret is
/// the `}` that closes it. An escaped `{{` opens nothing.
fn open_brace(chars: &[char], caret: usize) -> Option<usize> {
    if caret < 2 || chars.get(caret - 1) != Some(&'}') {
        return None;
    }
    let mut i = caret - 1;
    while i > 0 {
        i -= 1;
        match chars[i] {
            '{' => return opens_value(chars, i).then_some(i),
            '}' => return None,
            _ => {}
        }
    }
    None
}

/// Whether the `{` at `at` opens a value rather than being half of an escaped
/// `{{`. The template reads a run of braces in pairs from its left end, so the
/// last brace of a run opens a value when the run is odd: `{` and `{{{` do,
/// `{{` does not.
fn opens_value(chars: &[char], at: usize) -> bool {
    let Some(before) = at.checked_add(1).and_then(|end| chars.get(..end)) else {
        return false;
    };
    let run = before.iter().rev().take_while(|&&c| c == '{').count();
    run % 2 == 1
}

/// `text` as a template, if it is exactly one well-formed value and nothing
/// else.
fn single_value(text: &str) -> Option<Template> {
    let parsed = Template::parse(text);
    let one = matches!(parsed.parts(), [Part::Token(_)]);
    (one && parsed.problems().is_empty()).then_some(parsed)
}

/// What else is in one folder, from its whole listing less the names the card
/// is renaming — so a row keeping its own name does not collide with itself.
fn others_of(siblings: &[String], names: &[String]) -> HashSet<String> {
    let chosen: HashSet<&str> = names.iter().map(String::as_str).collect();
    siblings
        .iter()
        .filter(|name| !chosen.contains(name.as_str()))
        .cloned()
        .collect()
}

/// Each file as its folder and its name.
fn rows_of(paths: &[PathBuf]) -> Vec<(PathBuf, String)> {
    paths
        .iter()
        .filter_map(|path| {
            let folder = path.parent()?.to_path_buf();
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some((folder, name))
        })
        .collect()
}

/// What else is in each folder the rows are in, read from the disk: one
/// `read_dir` per folder. A folder that cannot be read contributes nothing,
/// and the rename itself is still refused by the kernel if a name is taken.
fn others_on_disk(rows: &[(PathBuf, String)]) -> HashMap<PathBuf, HashSet<String>> {
    let mut others: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    for (folder, _) in rows {
        if others.contains_key(folder) {
            continue;
        }
        let siblings: Vec<String> = std::fs::read_dir(folder)
            .map(|read| {
                read.filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let names: Vec<String> = rows
            .iter()
            .filter(|(at, _)| at == folder)
            .map(|(_, name)| name.clone())
            .collect();
        others.insert(folder.clone(), others_of(&siblings, &names));
    }
    others
}

/// The validation, as a pure function of the names and what else is in the
/// directory.
pub fn problems(names: &[&str], others: &HashSet<String>) -> Vec<Option<Problem>> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for name in names {
        *counts.entry(*name).or_default() += 1;
    }
    names
        .iter()
        .map(|name| {
            // `/` only: see plans/other-platforms/03-paths.md for the port.
            if name.is_empty() || *name == "." || *name == ".." || name.contains('/') {
                return Some(Problem::Unusable);
            }
            if name.len() > NAME_MAX {
                return Some(Problem::TooLong);
            }
            if counts.get(*name).copied().unwrap_or(0) > 1 {
                return Some(Problem::Duplicate);
            }
            if others.contains(*name) {
                return Some(Problem::Taken);
            }
            None
        })
        .collect()
}

/// Order a set of renames so that running them one at a time never collides.
///
/// The hard case is a swap — `a`→`b` and `b`→`a` — which no ordering can make
/// safe, because whichever runs first lands on a name the other still holds. So
/// the ones that *can* be ordered are, and every remaining cycle is broken by
/// sending its first member through a temporary name.
///
/// The temporary is the destination with a `.df-rename-N` suffix, which is a
/// name nothing else in this card can want (they are all real names the user
/// typed) and which is gone again by the end of the batch. The journal records
/// the *final* positions, so an undo never sees it.
pub fn ordered_renames(dir: &Path, pairs: &[(String, String)]) -> Vec<(PathBuf, PathBuf)> {
    let sources: HashSet<&str> = pairs.iter().map(|(from, _)| from.as_str()).collect();
    let mut pending: Vec<(String, String)> = pairs.to_vec();
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut freed: HashSet<String> = HashSet::new();
    let mut temporaries = 0usize;

    // Repeatedly take every rename whose destination is not still occupied by a
    // file this batch has yet to move.
    loop {
        let ready: Vec<(String, String)> = pending
            .iter()
            .filter(|(_, to)| !sources.contains(to.as_str()) || freed.contains(to.as_str()))
            .cloned()
            .collect();
        if ready.is_empty() {
            break;
        }
        for (from, to) in ready {
            out.push((dir.join(&from), dir.join(&to)));
            freed.insert(from.clone());
            pending.retain(|(f, _)| *f != from);
        }
    }

    // Whatever is left is one or more cycles. Break each by moving its first
    // member aside, which turns the cycle into a chain the loop above can drain.
    while !pending.is_empty() {
        let (from, to) = pending.remove(0);
        temporaries += 1;
        let temporary = format!("{to}.df-rename-{temporaries}");
        out.push((dir.join(&from), dir.join(&temporary)));
        freed.insert(from.clone());
        let mut deferred = vec![(temporary, to)];
        loop {
            let ready: Vec<(String, String)> = pending
                .iter()
                .filter(|(_, to)| !sources.contains(to.as_str()) || freed.contains(to.as_str()))
                .cloned()
                .collect();
            if ready.is_empty() {
                break;
            }
            for (from, to) in ready {
                out.push((dir.join(&from), dir.join(&to)));
                freed.insert(from.clone());
                pending.retain(|(f, _)| *f != from);
            }
        }
        out.extend(
            deferred
                .drain(..)
                .map(|(from, to)| (dir.join(from), dir.join(to))),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use df_core::keymap::parse_chord;

    // ── Driving ─────────────────────────────────────────────────────────────

    /// Bare characters are themselves; anything in angle brackets goes
    /// through the keymap's own parser, as in the editors' tests: `"cat"`,
    /// `"<ctrl+shift+down>"`, `"<tab>"`.
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

    /// Feed a script, returning the last outcome.
    fn run(bulk: &mut Bulk, script: &str) -> Outcome {
        let mut last = Outcome::Consumed;
        for chord in chords(script) {
            last = bulk.key(chord);
        }
        last
    }

    fn lines(bulk: &Bulk) -> Vec<&str> {
        bulk.editor.lines().iter().map(String::as_str).collect()
    }

    fn civil(year: i32, month: u32, day: u32) -> Civil {
        Civil {
            year,
            month,
            day,
            hour: 12,
            minute: 0,
            second: 0,
        }
    }

    /// A file that is not a photo, modified on the given day.
    fn file(name: &str, modified: Option<Civil>) -> Facts {
        Facts {
            name: name.to_string(),
            is_dir: false,
            size: 1,
            modified,
            created: None,
            parent: "x".to_string(),
            photo: Photo::None,
        }
    }

    /// A photo the reader has not got to yet.
    fn pending(name: &str) -> Facts {
        Facts {
            photo: Photo::Pending,
            ..file(name, Some(civil(2020, 1, 1)))
        }
    }

    fn taken(year: i32, month: u32, day: u32) -> Option<PhotoFacts> {
        Some(PhotoFacts {
            taken: Some(civil(year, month, day)),
            ..PhotoFacts::default()
        })
    }

    /// A photo the reader has already read, taken on the given day.
    fn read_photo(name: &str, year: i32, month: u32, day: u32) -> Facts {
        Facts {
            photo: taken(year, month, day).map_or(Photo::None, Photo::Some),
            ..file(name, Some(civil(2020, 1, 1)))
        }
    }

    fn card_of(facts: Vec<Facts>, siblings: &[&str]) -> Bulk {
        let siblings: Vec<String> = siblings.iter().map(|s| s.to_string()).collect();
        Bulk::from_facts(PathBuf::from("/tmp/x"), facts, &siblings)
    }

    fn card(names: &[&str], siblings: &[&str]) -> Bulk {
        card_of(
            names.iter().map(|name| file(name, None)).collect(),
            siblings,
        )
    }

    /// Replace the template's text as a person would: select it all, type.
    fn retype(bulk: &mut Bulk, template: &str) {
        bulk.focus = Focus::Template;
        run(bulk, "<ctrl+a><ctrl+k>");
        bulk.insert_text(template);
        bulk.popover = None;
    }

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // ── Opening ─────────────────────────────────────────────────────────────

    /// **The bug this fixes**: `r` with two rows selected opened the bulk card
    /// wherever the pane was — including on a remote service, where `dir` is
    /// the display path `sftp://host/srv`. Committing then called `rename(2)`
    /// on `sftp:/host/srv/name`, a *relative* path resolved against the
    /// process's own working directory: a rename of files nobody was looking
    /// at, or forty error toasts. The card cannot be built pointed at a server.
    #[test]
    fn the_card_cannot_be_opened_over_a_link() {
        let names = vec!["a.txt".to_string(), "b.txt".to_string()];
        let refused = Bulk::new(
            PathBuf::from("sftp://showandtour1/srv/www"),
            names.clone(),
            &[],
            df_core::fs::no_notifier(),
        );
        let why = refused
            .err()
            .expect("a remote directory has no bulk rename");
        assert!(why.contains('r'), "the notice names the way out: {why}");

        // A local directory is unaffected.
        let ok = Bulk::new(
            PathBuf::from("/tmp/somewhere"),
            names,
            &[],
            df_core::fs::no_notifier(),
        )
        .expect("a local card");
        assert_eq!(ok.len(), 2);
        assert_eq!(
            lines(&ok),
            ["a.txt", "b.txt"],
            "the default is the identity"
        );
    }

    /// Only a file with a photo's extension waits on the reader; a folder and a
    /// text file are settled the moment the card opens, so a card of them
    /// starts no worker at all.
    #[test]
    fn only_photos_wait_for_the_reader() {
        let tree = df_core::test_support::TempTree::new("bulk-photos");
        for name in ["notes.txt", "IMG_1.JPG", "raw.nef"] {
            std::fs::write(tree.path().join(name), b"not really").expect("write");
        }
        tree.dir("album.jpg");
        let names: Vec<String> = ["notes.txt", "album.jpg"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let plain = Bulk::new(
            tree.path().to_path_buf(),
            names,
            &[],
            df_core::fs::no_notifier(),
        )
        .expect("local");
        assert!(plain.facts.iter().all(|f| f.photo == Photo::None));
        assert!(plain.photos.is_none(), "no photos, no worker");

        let names: Vec<String> = ["IMG_1.JPG", "raw.nef", "notes.txt"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut photos = Bulk::new(
            tree.path().to_path_buf(),
            names,
            &[],
            df_core::fs::no_notifier(),
        )
        .expect("local");
        assert_eq!(photos.facts[0].photo, Photo::Pending);
        assert_eq!(photos.facts[1].photo, Photo::Pending);
        assert_eq!(photos.facts[2].photo, Photo::None);
        // Facts change only when the card polls, so this is not a race.
        retype(&mut photos, "{taken}{ext}");
        assert_eq!(
            photos.problems(),
            vec![
                Some(Problem::Pending),
                Some(Problem::Pending),
                Some(Problem::Missing("no date taken"))
            ]
        );
        // Neither file is really a photo, and the reader says so; the rows it
        // was holding up say what they are missing instead.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while photos.facts.iter().any(|f| f.photo == Photo::Pending) {
            assert!(
                std::time::Instant::now() < deadline,
                "the reader never answered"
            );
            photos.poll();
            std::thread::yield_now();
        }
        assert!(photos.facts.iter().all(|f| f.photo == Photo::None));
        assert_eq!(
            photos.problems(),
            vec![Some(Problem::Missing("no date taken")); 3]
        );
    }

    // ── Validation ──────────────────────────────────────────────────────────

    /// The ways a name can be wrong, each reported as itself.
    #[test]
    fn every_kind_of_bad_name_is_caught_and_named() {
        let others = set(&["taken.txt"]);
        let names = vec!["", "..", "a/b", "same", "same", "taken.txt", "fine.txt"];
        let found = problems(&names, &others);
        assert_eq!(found[0], Some(Problem::Unusable), "empty");
        assert_eq!(found[1], Some(Problem::Unusable), "dot dot");
        assert_eq!(found[2], Some(Problem::Unusable), "slash");
        // Both halves of a duplicate are marked: neither is wrong on its own,
        // and the fix is a choice between them.
        assert_eq!(found[3], Some(Problem::Duplicate));
        assert_eq!(found[4], Some(Problem::Duplicate));
        assert_eq!(found[5], Some(Problem::Taken));
        assert_eq!(found[6], None);

        let long = "x".repeat(NAME_MAX + 1);
        assert_eq!(
            problems(&[long.as_str()], &HashSet::new())[0],
            Some(Problem::TooLong)
        );
        assert_eq!(
            problems(&["x".repeat(NAME_MAX).as_str()], &HashSet::new())[0],
            None
        );
    }

    /// A file being renamed away does not collide with itself, which is what
    /// makes a swap legal in this card.
    #[test]
    fn the_files_being_renamed_are_not_their_own_obstacles() {
        let bulk = card(&["a.txt", "b.txt"], &["a.txt", "b.txt", "c.txt"]);
        assert!(bulk.valid(), "nothing has been changed yet");
        assert_eq!(bulk.changes(), 0);
        // Renaming onto a name that is *not* in the card is still refused.
        let names = vec!["c.txt", "b.txt"];
        assert_eq!(problems(&names, &set(&["c.txt"]))[0], Some(Problem::Taken));
    }

    // ── The template ────────────────────────────────────────────────────────

    /// Every row is the template resolved for its own file, and a row the
    /// template wrote is untouched.
    #[test]
    fn the_template_rewrites_every_row_and_leaves_them_untouched() {
        let mut bulk = card(&["IMG_1.jpg", "IMG_2.jpg", "notes.txt"], &[]);
        assert_eq!(bulk.focus, Focus::Template, "the card opens in the field");
        // Typing at the end of `{name}{ext}`.
        run(&mut bulk, "<home>x-");
        assert_eq!(bulk.template.text(), "x-{name}{ext}");
        assert_eq!(lines(&bulk), ["x-IMG_1.jpg", "x-IMG_2.jpg", "x-notes.txt"]);
        assert!((0..3).all(|row| bulk.untouched(row)));
        assert_eq!(bulk.derived[2], Ok("x-notes.txt".to_string()));
        assert_eq!(bulk.changes(), 3);
        assert!(bulk.valid());

        // Undo in the field takes the rows back with it.
        run(&mut bulk, "<ctrl+z>");
        assert_eq!(
            bulk.template.text(),
            "{name}{ext}",
            "the field undoes itself"
        );
        assert_eq!(lines(&bulk), ["IMG_1.jpg", "IMG_2.jpg", "notes.txt"]);
    }

    /// A photo landing rewrites the rows it can, but not one somebody has
    /// typed into; a template edit rewrites that one too.
    #[test]
    fn a_hand_edit_survives_a_photo_arriving_but_not_a_template_edit() {
        let mut bulk = card_of(vec![pending("a.jpg"), pending("b.jpg")], &[]);
        retype(&mut bulk, "{taken}{ext}");
        assert_eq!(
            lines(&bulk),
            ["a.jpg", "b.jpg"],
            "waiting keeps the old lines"
        );
        assert_eq!(bulk.derived[0], Err(Missing::Pending));

        // Row 1 by hand: `End`, then a word.
        run(&mut bulk, "<tab><down><end>-mine");
        assert_eq!(lines(&bulk)[1], "b.jpg-mine");
        assert!(!bulk.untouched(1));

        bulk.photo_arrived(0, taken(2024, 5, 6));
        bulk.photo_arrived(1, taken(2023, 1, 2));
        bulk.refresh();
        assert_eq!(lines(&bulk), ["2024-05-06.jpg", "b.jpg-mine"]);

        run(&mut bulk, "<tab>");
        assert_eq!(bulk.focus, Focus::Template);
        run(&mut bulk, "<home>x");
        assert_eq!(lines(&bulk), ["x2024-05-06.jpg", "x2023-01-02.jpg"]);
    }

    /// The template's rewrite is one step of the rows' undo, so the hand
    /// edits it threw away come back with `Ctrl+z` in the rows.
    #[test]
    fn ctrl_z_in_the_rows_brings_back_what_a_template_edit_replaced() {
        let mut bulk = card(&["a.txt", "b.txt", "c.txt"], &[]);
        run(
            &mut bulk,
            "<tab><ctrl+shift+down><ctrl+shift+down><home>old-<esc>",
        );
        assert_eq!(lines(&bulk), ["old-a.txt", "old-b.txt", "old-c.txt"]);
        run(&mut bulk, "<tab><home>new-");
        assert_eq!(lines(&bulk), ["new-a.txt", "new-b.txt", "new-c.txt"]);
        run(&mut bulk, "<tab><ctrl+z>");
        assert_eq!(lines(&bulk), ["old-a.txt", "old-b.txt", "old-c.txt"]);
    }

    /// A row the new template cannot fill shows what the file is called now,
    /// not a hand edit the wipe would otherwise have spared, and the hand
    /// edit is one `Ctrl+z` away.
    #[test]
    fn a_row_the_template_cannot_fill_shows_its_old_name() {
        let mut bulk = card(&["a.png", "b.png"], &[]);
        run(&mut bulk, "<tab><end>-mine");
        assert_eq!(lines(&bulk), ["a.png-mine", "b.png"]);
        retype(&mut bulk, "{taken}{ext}");
        assert_eq!(lines(&bulk), ["a.png", "b.png"], "the old names");
        assert!(bulk.untouched(0), "the card put the old name there");
        assert_eq!(
            bulk.problems(),
            vec![Some(Problem::Missing("no date taken")); 2],
            "and says why"
        );
        run(&mut bulk, "<tab><ctrl+z>");
        assert_eq!(lines(&bulk), ["a.png-mine", "b.png"]);
    }

    /// A photo landing mid-word is in no undo step and does not split the
    /// word. So `Ctrl+z` never takes back only the photo: it takes back the
    /// whole word, and the row the photo filled in keeps its taken date.
    #[test]
    fn undo_after_a_photo_arrives_takes_back_the_word_and_keeps_the_photo() {
        let mut bulk = card_of(vec![pending("a.jpg"), read_photo("b.jpg", 2023, 1, 2)], &[]);
        retype(&mut bulk, "{taken}{ext}");
        assert_eq!(lines(&bulk), ["a.jpg", "2023-01-02.jpg"]);

        run(&mut bulk, "<tab><down><end>ab");
        bulk.photo_arrived(0, taken(2024, 5, 6));
        bulk.refresh();
        assert_eq!(lines(&bulk), ["2024-05-06.jpg", "2023-01-02.jpgab"]);
        run(&mut bulk, "cd");
        assert_eq!(lines(&bulk)[1], "2023-01-02.jpgabcd");

        run(&mut bulk, "<ctrl+z>");
        assert_eq!(
            lines(&bulk),
            ["2024-05-06.jpg", "2023-01-02.jpg"],
            "the word went in one step, and the photo's name stayed"
        );
        assert!(bulk.untouched(0) && bulk.untouched(1));
    }

    /// `Ctrl+z` in the rows after a template edit brings back what a person
    /// had typed into the rows. The rows nobody typed into keep following the
    /// template, because the field still says it: they show what it makes of
    /// them now, photo included. That holds whether the photo landed as part
    /// of the template's rewrite or quietly after a caret moved in the rows.
    #[test]
    fn undo_after_a_template_edit_brings_back_only_the_hand_typed_rows() {
        for visit in ["<tab>", "<tab><down>"] {
            let mut bulk = card_of(
                vec![
                    pending("a.jpg"),
                    read_photo("b.jpg", 2023, 1, 2),
                    file("c.txt", None),
                ],
                &[],
            );
            run(&mut bulk, "<tab><down><down><end>-mine");
            assert_eq!(lines(&bulk)[2], "c.txt-mine");
            retype(&mut bulk, "{taken}{ext}");
            assert_eq!(lines(&bulk), ["a.jpg", "2023-01-02.jpg", "c.txt"]);
            run(&mut bulk, visit);
            bulk.photo_arrived(0, taken(2024, 5, 6));
            bulk.refresh();
            assert_eq!(
                lines(&bulk),
                ["2024-05-06.jpg", "2023-01-02.jpg", "c.txt"],
                "{visit}"
            );

            run(&mut bulk, "<ctrl+z>");
            assert_eq!(
                lines(&bulk),
                ["2024-05-06.jpg", "2023-01-02.jpg", "c.txt-mine"],
                "{visit}: the hand edit is back, the template's rows follow it"
            );
            assert!(bulk.untouched(0) && bulk.untouched(1), "{visit}");
            assert!(!bulk.untouched(2), "{visit}");
            assert_eq!(bulk.problems(), vec![None, None, None], "{visit}");

            run(&mut bulk, "<ctrl+y>");
            assert_eq!(
                lines(&bulk),
                ["2024-05-06.jpg", "2023-01-02.jpg", "c.txt"],
                "{visit}"
            );
            assert_eq!(
                bulk.problems()[2],
                Some(Problem::Missing("no date taken")),
                "{visit}: the template's again"
            );
        }
    }

    /// An undo that reaches back past a photo's arrival puts back a line
    /// written while the row was still waiting. The row is the template's
    /// again, so the card fills in the taken date it now knows rather than
    /// leaving the old name standing as if somebody had typed it.
    #[test]
    fn undo_past_a_photo_arriving_fills_the_row_in_again() {
        let mut bulk = card_of(vec![pending("a.jpg"), read_photo("b.jpg", 2023, 1, 2)], &[]);
        retype(&mut bulk, "{taken}{ext}");
        run(&mut bulk, "<tab><down><end>ab");
        bulk.photo_arrived(0, taken(2024, 5, 6));
        bulk.refresh();
        assert_eq!(lines(&bulk), ["2024-05-06.jpg", "2023-01-02.jpgab"]);

        run(&mut bulk, "<ctrl+z>");
        assert_eq!(lines(&bulk), ["2024-05-06.jpg", "2023-01-02.jpg"]);
        assert!(bulk.untouched(0) && bulk.untouched(1));
        assert_eq!(bulk.problems(), vec![None, None]);
        assert!(bulk.valid());

        run(&mut bulk, "<ctrl+y>");
        assert_eq!(lines(&bulk), ["2024-05-06.jpg", "2023-01-02.jpgab"]);
        assert!(bulk.untouched(0) && !bulk.untouched(1));
    }

    /// Undoing a row move puts the rows, their old names and their numbers
    /// back, and leaves them the template's rows rather than hand edits.
    #[test]
    fn undoing_a_row_move_leaves_the_rows_untouched() {
        let mut bulk = card(&["a.txt", "b.txt", "c.txt"], &[]);
        retype(&mut bulk, "{n}-{name}{ext}");
        run(&mut bulk, "<tab><alt+down>");
        assert_eq!(lines(&bulk), ["1-b.txt", "2-a.txt", "3-c.txt"]);
        run(&mut bulk, "<ctrl+z>");
        assert_eq!(lines(&bulk), ["1-a.txt", "2-b.txt", "3-c.txt"]);
        assert_eq!(bulk.olds, ["a.txt", "b.txt", "c.txt"]);
        assert!((0..3).all(|row| bulk.untouched(row)));

        // A photo or a template edit after it still reaches every row.
        run(&mut bulk, "<tab><home>x");
        assert_eq!(lines(&bulk), ["x1-a.txt", "x2-b.txt", "x3-c.txt"]);
    }

    /// The card resolves every row against one parse of the field, kept from
    /// the last edit. Every way the text can change parses it again.
    #[test]
    fn the_parsed_template_follows_the_field() {
        fn in_step(bulk: &Bulk) -> bool {
            format!("{:?}", bulk.parsed()) == format!("{:?}", Template::parse(bulk.template.text()))
        }
        let mut bulk = card_of(vec![file("a.txt", Some(civil(2021, 3, 4)))], &[]);
        assert!(in_step(&bulk), "on opening");
        run(&mut bulk, "<home>x-");
        assert!(in_step(&bulk), "typing");
        run(&mut bulk, "<ctrl+z>");
        assert!(in_step(&bulk), "undo");
        run(&mut bulk, "<home>{mod<tab>");
        assert_eq!(bulk.template.text(), "{modified}{name}{ext}");
        assert!(in_step(&bulk), "a completion");
        run(&mut bulk, "<home><shift+right><ctrl+x>");
        assert!(in_step(&bulk), "a cut");
        bulk.insert_text("{");
        assert!(in_step(&bulk), "a paste");
        let back = Bulk::reopen(
            Box::new(bulk),
            Path::new("/tmp/x"),
            &["a.txt".to_string()],
            &[],
        )
        .expect("the same selection");
        assert!(in_step(&back), "a draft taken back");
    }

    // ── Values typed into the rows ──────────────────────────────────────────

    /// A `}` closing a value in the rows resolves it at every caret, each for
    /// its own file, and the braces do not stay.
    #[test]
    fn a_closing_brace_resolves_the_value_per_row() {
        let mut bulk = card_of(
            vec![
                file("a.txt", Some(civil(2021, 3, 4))),
                file("b.txt", Some(civil(2022, 5, 6))),
                file("c.txt", None),
            ],
            &[],
        );
        run(&mut bulk, "<tab><ctrl+shift+down><ctrl+shift+down><home>");
        bulk.popover = None;
        run(&mut bulk, "{date<esc>}_");
        assert_eq!(
            lines(&bulk),
            ["2021-03-04_a.txt", "2022-05-06_b.txt", "{date}_c.txt"],
            "a file with no date keeps the literal text"
        );
    }

    /// `{n}` counts rows by where they are, so a row moved with `Alt+↓` takes
    /// the number of its new place, and its old name, facts and template
    /// result travel with it.
    #[test]
    fn the_counter_numbers_by_row_and_follows_a_row_move() {
        let mut bulk = card(&["a.txt", "b.txt", "c.txt"], &[]);
        retype(&mut bulk, "{n}-{name}{ext}");
        assert_eq!(lines(&bulk), ["1-a.txt", "2-b.txt", "3-c.txt"]);
        run(&mut bulk, "<tab><alt+down>");
        assert_eq!(bulk.olds, ["b.txt", "a.txt", "c.txt"]);
        assert_eq!(bulk.facts[1].name, "a.txt");
        assert_eq!(lines(&bulk), ["1-b.txt", "2-a.txt", "3-c.txt"]);
        assert_eq!(bulk.derived[1], Ok("2-a.txt".to_string()));
        let renames = bulk.renames();
        assert!(renames.contains(&(
            PathBuf::from("/tmp/x/a.txt"),
            PathBuf::from("/tmp/x/2-a.txt")
        )));
        assert!(renames.contains(&(
            PathBuf::from("/tmp/x/b.txt"),
            PathBuf::from("/tmp/x/1-b.txt")
        )));
    }

    // ── Missing and pending ─────────────────────────────────────────────────

    /// A value the file cannot answer marks the rows the template wrote, and
    /// not a row somebody has since named by hand.
    #[test]
    fn missing_marks_only_untouched_rows() {
        let mut bulk = card(&["a.png", "b.png", "c.png"], &[]);
        retype(&mut bulk, "{taken}{ext}");
        let found = bulk.problems();
        assert_eq!(found, vec![Some(Problem::Missing("no date taken")); 3]);
        assert_eq!(found[0].map(Problem::message), Some("no date taken"));
        run(&mut bulk, "<tab><down><end>2");
        let found = bulk.problems();
        assert_eq!(found[1], None, "a row named by hand says what it wants");
        assert_eq!(found[0], Some(Problem::Missing("no date taken")));
        assert!(!bulk.valid());
    }

    /// `Enter` waits for the photo reader, without moving anything, and goes
    /// once it is done.
    #[test]
    fn a_pending_photo_holds_enter() {
        let mut bulk = card_of(
            vec![pending("a.jpg"), file("b.txt", Some(civil(2021, 1, 1)))],
            &[],
        );
        retype(&mut bulk, "{date}{ext}");
        assert_eq!(bulk.problems(), vec![Some(Problem::Pending), None]);
        assert_eq!(
            bulk.problems()[0].map(Problem::message),
            Some("reading photo…")
        );
        assert_eq!(run(&mut bulk, "<enter>"), Outcome::Consumed);
        run(&mut bulk, "<tab><down>");
        let before = bulk.editor.primary();
        assert_eq!(run(&mut bulk, "<enter>"), Outcome::Consumed);
        assert_eq!(bulk.editor.primary(), before, "a wait moves no caret");
        bulk.photo_arrived(0, taken(2024, 5, 6));
        bulk.refresh();
        assert_eq!(lines(&bulk), ["2024-05-06.jpg", "2021-01-01.txt"]);
        assert_eq!(run(&mut bulk, "<enter>"), Outcome::Submit);
    }

    // ── The popover ─────────────────────────────────────────────────────────

    /// `{` opens the list, typing narrows it, `Tab` takes the highlighted
    /// value: literally in the template, resolved in the rows.
    #[test]
    fn the_popover_opens_on_a_brace_filters_and_accepts() {
        let mut bulk = card_of(
            vec![
                file("a.txt", Some(civil(2021, 3, 4))),
                file("b.txt", Some(civil(2022, 5, 6))),
            ],
            &[],
        );
        run(&mut bulk, "<home>{");
        let popover = bulk.live_popover().expect("a `{` opens the list");
        assert_eq!(popover.anchor_row, None);
        assert_eq!(popover.open_at, 0);
        assert_eq!(popover.candidates[0].insert, "{name}");
        run(&mut bulk, "mod");
        let popover = bulk.live_popover().expect("still open");
        assert_eq!(popover.candidates[0].insert, "{modified}");
        assert_eq!(
            bulk.preview(&popover.candidates[0]),
            Ok("2021-03-04".to_string()),
            "the preview is row 0's"
        );
        run(&mut bulk, "<tab>");
        assert!(bulk.popover.is_none(), "accepting closes it");
        assert_eq!(bulk.template.text(), "{modified}{name}{ext}");
        assert_eq!(lines(&bulk), ["2021-03-04a.txt", "2022-05-06b.txt"]);

        // `{{` is a literal brace and asks for nothing.
        run(&mut bulk, "<home>{{");
        assert!(bulk.popover.is_none());
        run(&mut bulk, "<backspace><backspace>");

        // In the rows: every caret, each resolved for its own row.
        run(&mut bulk, "<tab><ctrl+shift+down><home>{da");
        let popover = bulk.live_popover().expect("a `{` in the rows");
        assert_eq!(
            popover.anchor_row,
            Some(1),
            "the primary is the newest caret"
        );
        assert_eq!(popover.candidates[0].insert, "{date}");
        run(&mut bulk, "<down><up><tab>");
        assert_eq!(
            lines(&bulk),
            ["2021-03-042021-03-04a.txt", "2022-05-062022-05-06b.txt"]
        );

        // Moving out of the value closes the list.
        run(&mut bulk, "{<left>");
        assert!(bulk.popover.is_none());
        // …and `Esc` closes only the list.
        run(&mut bulk, "<right><backspace>{");
        assert!(bulk.live_popover().is_some());
        assert_eq!(run(&mut bulk, "<esc>"), Outcome::Consumed);
        assert!(bulk.popover.is_none());
    }

    /// `{{` is a literal brace, so the third `{` of `{{{` is an opener and
    /// asks for the list, in the field and in the rows.
    #[test]
    fn three_braces_are_a_literal_brace_and_an_opener() {
        let mut bulk = card(&["a.txt", "b.txt"], &[]);
        run(&mut bulk, "<home>{{");
        assert!(bulk.popover.is_none(), "{{{{ asks for nothing");
        run(&mut bulk, "{");
        let popover = bulk.live_popover().expect("the third brace opens");
        assert_eq!(popover.open_at, 2);
        run(&mut bulk, "{");
        assert!(bulk.popover.is_none(), "and a fourth is literal again");

        let mut bulk = card(&["a.txt", "b.txt"], &[]);
        run(&mut bulk, "<tab><home>{{{");
        let popover = bulk.live_popover().expect("in the rows too");
        assert_eq!((popover.anchor_row, popover.open_at), (Some(0), 2));
    }

    /// The popover's arrows wrap and keep the highlight on screen.
    #[test]
    fn the_popover_selection_wraps_and_scrolls() {
        let mut bulk = card(&["a.txt", "b.txt"], &[]);
        run(&mut bulk, "{");
        let count = bulk.live_popover().expect("open").candidates.len();
        assert!(count > POPOVER_ROWS);
        run(&mut bulk, "<up>");
        let popover = bulk.live_popover().expect("open");
        assert_eq!(popover.selected, count - 1, "up from the top is the bottom");
        assert_eq!(popover.first, count - POPOVER_ROWS);
        run(&mut bulk, "<down>");
        let popover = bulk.live_popover().expect("open");
        assert_eq!((popover.selected, popover.first), (0, 0));
    }

    // ── Moving about ────────────────────────────────────────────────────────

    /// `Ctrl+↓` and `Ctrl+↑` go to the next and previous rows with something
    /// wrong, wrapping; with nothing wrong they do nothing.
    #[test]
    fn ctrl_arrows_jump_between_problem_rows() {
        let mut bulk = card(&["a", "b", "c", "d", "e"], &[]);
        run(&mut bulk, "<tab>");
        run(&mut bulk, "<ctrl+down>");
        assert_eq!(bulk.editor.primary().head.row, 0, "nothing to jump to");
        // Rows 1 and 3 both become `x`.
        run(
            &mut bulk,
            "<down><ctrl+a><ctrl+k>x<down><down><ctrl+a><ctrl+k>x",
        );
        run(&mut bulk, "<ctrl+home>");
        run(&mut bulk, "<ctrl+down>");
        assert_eq!(bulk.editor.primary().head.row, 1);
        run(&mut bulk, "<ctrl+down>");
        assert_eq!(bulk.editor.primary().head.row, 3);
        run(&mut bulk, "<ctrl+down>");
        assert_eq!(bulk.editor.primary().head.row, 1, "wraps to the first");
        run(&mut bulk, "<ctrl+up>");
        assert_eq!(bulk.editor.primary().head.row, 3, "and back to the last");
    }

    /// A row waiting on the photo reader is not somewhere `Ctrl+↑/↓` goes:
    /// nothing there needs fixing.
    #[test]
    fn ctrl_arrows_skip_rows_waiting_on_a_photo() {
        let mut bulk = card_of(
            vec![
                pending("a.jpg"),
                file("b.txt", None),
                pending("c.jpg"),
                file("d.txt", None),
            ],
            &[],
        );
        retype(&mut bulk, "{taken}{ext}");
        assert_eq!(bulk.problems()[0], Some(Problem::Pending));
        assert_eq!(bulk.problems()[2], Some(Problem::Pending));
        run(&mut bulk, "<tab>");
        assert_eq!(bulk.editor.primary().head.row, 0);
        run(&mut bulk, "<ctrl+down>");
        assert_eq!(bulk.editor.primary().head.row, 1);
        run(&mut bulk, "<ctrl+down>");
        assert_eq!(bulk.editor.primary().head.row, 3, "past the wait on 2");
        run(&mut bulk, "<ctrl+down>");
        assert_eq!(bulk.editor.primary().head.row, 1, "wraps past 0");
        run(&mut bulk, "<ctrl+up>");
        assert_eq!(bulk.editor.primary().head.row, 3);
    }

    /// `↑` on the top row with a selection drops the selection, as it would
    /// on any row; the press after it leaves for the field.
    #[test]
    fn up_on_the_top_row_collapses_a_selection_before_leaving() {
        let mut bulk = card(&["abc", "def"], &[]);
        run(&mut bulk, "<tab><shift+home>");
        assert!(bulk.editor.primary().is_selection());
        run(&mut bulk, "<up>");
        assert_eq!(bulk.focus, Focus::Rows, "the first press stays");
        assert!(!bulk.editor.primary().is_selection());
        run(&mut bulk, "<up>");
        assert_eq!(bulk.focus, Focus::Template);
    }

    /// `Tab` and `↓` go down into the rows, `Tab` and `↑` from the top row
    /// come back, and `Enter` from the rows goes to a problem instead of
    /// submitting.
    #[test]
    fn the_keyboard_moves_between_the_field_and_the_rows() {
        let mut bulk = card(&["a", "b"], &["taken"]);
        run(&mut bulk, "<down>");
        assert_eq!(bulk.focus, Focus::Rows);
        run(&mut bulk, "<up>");
        assert_eq!(bulk.focus, Focus::Template);
        run(&mut bulk, "<tab><down><ctrl+a><ctrl+k>taken<up>");
        assert_eq!(bulk.focus, Focus::Rows, "only from the top row");
        assert_eq!(run(&mut bulk, "<enter>"), Outcome::Consumed);
        assert_eq!(
            bulk.editor.primary().head.row,
            1,
            "Enter goes to the problem"
        );
        run(&mut bulk, "<ctrl+a><ctrl+k>c");
        assert_eq!(run(&mut bulk, "<enter>"), Outcome::Submit);
        assert_eq!(run(&mut bulk, "<esc>"), Outcome::Close);
    }

    // ── Copy and cut ────────────────────────────────────────────────────────

    /// `Ctrl+c` at two carets copies both selections, one to a line, and
    /// changes nothing. `Ctrl+x` copies the same and deletes them as one step.
    #[test]
    fn copy_and_cut_in_the_rows_take_every_selection() {
        let mut bulk = card(&["cat.jpg", "dog.jpg"], &[]);
        run(
            &mut bulk,
            "<tab><home><ctrl+shift+down><shift+right><shift+right><shift+right>",
        );
        assert_eq!(
            run(&mut bulk, "<ctrl+c>"),
            Outcome::Copy("cat\ndog".to_string())
        );
        assert_eq!(lines(&bulk), ["cat.jpg", "dog.jpg"]);
        assert_eq!(bulk.editor.cursors().len(), 2, "the carets are still there");
        assert!(
            bulk.editor.primary().is_selection(),
            "and so are the selections"
        );

        assert_eq!(
            run(&mut bulk, "<ctrl+x>"),
            Outcome::Copy("cat\ndog".to_string())
        );
        assert_eq!(lines(&bulk), [".jpg", ".jpg"]);
        assert!(!bulk.untouched(0), "a cut is a hand edit");
        run(&mut bulk, "<ctrl+z>");
        assert_eq!(lines(&bulk), ["cat.jpg", "dog.jpg"], "in one step");
    }

    /// With nothing selected there is nothing to copy. The press does nothing
    /// at all, and above all it does not close the card, which is what
    /// `Ctrl+c` used to do.
    #[test]
    fn copy_and_cut_with_nothing_selected_do_nothing() {
        let mut bulk = card(&["cat.jpg", "dog.jpg"], &[]);
        for key in ["<ctrl+c>", "<ctrl+x>"] {
            assert_eq!(run(&mut bulk, key), Outcome::Consumed, "field {key}");
            assert_eq!(bulk.template.text(), "{name}{ext}");
        }
        run(&mut bulk, "<tab><ctrl+shift+down>");
        for key in ["<ctrl+c>", "<ctrl+x>"] {
            assert_eq!(run(&mut bulk, key), Outcome::Consumed, "rows {key}");
            assert_eq!(lines(&bulk), ["cat.jpg", "dog.jpg"]);
            assert_eq!(bulk.editor.cursors().len(), 2, "rows {key}");
        }
    }

    /// The template field copies and cuts its own selection. A cut changes
    /// the template, so the rows follow it, and the field's undo brings both
    /// back.
    #[test]
    fn copy_and_cut_in_the_field_take_its_selection() {
        let mut bulk = card(&["cat.jpg"], &[]);
        run(
            &mut bulk,
            "<home><shift+right><shift+right><shift+right><shift+right><shift+right><shift+right>",
        );
        assert_eq!(
            run(&mut bulk, "<ctrl+c>"),
            Outcome::Copy("{name}".to_string())
        );
        assert_eq!(bulk.template.text(), "{name}{ext}");
        assert_eq!(
            run(&mut bulk, "<ctrl+x>"),
            Outcome::Copy("{name}".to_string())
        );
        assert_eq!(bulk.template.text(), "{ext}");
        assert_eq!(lines(&bulk), [".jpg"]);
        run(&mut bulk, "<ctrl+z>");
        assert_eq!(bulk.template.text(), "{name}{ext}");
        assert_eq!(lines(&bulk), ["cat.jpg"]);
    }

    // ── The draft ───────────────────────────────────────────────────────────

    /// A card closed with names in it comes back for the same selection, in
    /// the order it was left, with the directory read again; a different
    /// selection starts fresh.
    #[test]
    fn a_draft_comes_back_for_the_same_selection_only() {
        let mut bulk = card(&["a.txt", "b.txt"], &[]);
        run(&mut bulk, "<tab><alt+down><tab><home>x");
        let draft = Box::new(bulk);
        let names = |list: &[&str]| -> Vec<String> { list.iter().map(|s| s.to_string()).collect() };

        let other_dir = Bulk::reopen(
            draft,
            Path::new("/tmp/elsewhere"),
            &names(&["a.txt", "b.txt"]),
            &[],
        );
        assert!(other_dir.is_none());

        let mut bulk = card(&["a.txt", "b.txt"], &[]);
        run(&mut bulk, "<tab><alt+down><tab><home>x");
        let draft = Box::new(bulk);
        let fewer = Bulk::reopen(draft, Path::new("/tmp/x"), &names(&["a.txt"]), &[]);
        assert!(fewer.is_none());

        let mut bulk = card(&["a.txt", "b.txt"], &[]);
        run(&mut bulk, "<tab><alt+down><tab><home>x");
        let draft = Box::new(bulk);
        let back = Bulk::reopen(
            draft,
            Path::new("/tmp/x"),
            &names(&["a.txt", "b.txt"]),
            &names(&["a.txt", "b.txt", "xa.txt"]),
        )
        .expect("the same selection");
        assert_eq!(back.olds, ["b.txt", "a.txt"], "the draft's own order");
        assert_eq!(lines(&back), ["xb.txt", "xa.txt"]);
        assert_eq!(
            back.problems()[1],
            Some(Problem::Taken),
            "the directory is read again"
        );
    }

    // ── The commit ──────────────────────────────────────────────────────────

    /// A plain batch runs in an order where nothing lands on an occupied name.
    #[test]
    fn renames_are_ordered_so_nothing_collides() {
        let dir = Path::new("/d");
        // `b` must move out of the way before `a` can become `b`.
        let out = ordered_renames(
            dir,
            &[
                ("a".to_string(), "b".to_string()),
                ("b".to_string(), "c".to_string()),
            ],
        );
        assert_eq!(
            out,
            vec![
                (dir.join("b"), dir.join("c")),
                (dir.join("a"), dir.join("b")),
            ]
        );
    }

    /// A swap is the case no ordering can solve, so one leg goes through a
    /// temporary — and the temporary is gone by the end.
    #[test]
    fn a_swap_goes_through_a_temporary_that_does_not_survive() {
        let dir = Path::new("/d");
        let out = ordered_renames(
            dir,
            &[
                ("a".to_string(), "b".to_string()),
                ("b".to_string(), "a".to_string()),
            ],
        );
        assert_eq!(out.len(), 3, "two renames and one detour: {out:?}");
        // Nothing is left standing on a temporary name.
        let finals: HashSet<_> = out.iter().map(|(_, to)| to.clone()).collect();
        let sources: HashSet<_> = out.iter().map(|(from, _)| from.clone()).collect();
        for (_, to) in &out {
            let name = to.file_name().unwrap_or_default().to_string_lossy();
            if name.contains(".df-rename-") {
                assert!(sources.contains(to), "the temporary is moved on again");
            }
        }
        assert!(finals.contains(&dir.join("a")));
        assert!(finals.contains(&dir.join("b")));
        // Every step's source is free by the time it runs.
        let mut held: HashSet<PathBuf> = [dir.join("a"), dir.join("b")].into_iter().collect();
        for (from, to) in &out {
            assert!(held.remove(from), "{from:?} is not there to move");
            assert!(held.insert(to.clone()), "{to:?} is occupied");
        }
        assert_eq!(held, [dir.join("a"), dir.join("b")].into_iter().collect());
    }

    /// A three-way rotation is the same problem one size up.
    #[test]
    fn a_rotation_of_three_also_resolves() {
        let dir = Path::new("/d");
        let out = ordered_renames(
            dir,
            &[
                ("a".to_string(), "b".to_string()),
                ("b".to_string(), "c".to_string()),
                ("c".to_string(), "a".to_string()),
            ],
        );
        let mut held: HashSet<PathBuf> = [dir.join("a"), dir.join("b"), dir.join("c")]
            .into_iter()
            .collect();
        for (from, to) in &out {
            assert!(held.remove(from), "{from:?} is not there to move");
            assert!(held.insert(to.clone()), "{to:?} is occupied");
        }
        assert_eq!(
            held,
            [dir.join("a"), dir.join("b"), dir.join("c")]
                .into_iter()
                .collect()
        );
    }

    /// Rows that did not change are not renamed at all, and a swap of two
    /// names in the card is legal.
    #[test]
    fn unchanged_rows_produce_no_work() {
        let mut bulk = card(&["a.txt", "b.txt"], &[]);
        run(&mut bulk, "<tab><ctrl+a><ctrl+k>z.txt");
        assert_eq!(
            bulk.renames(),
            vec![(PathBuf::from("/tmp/x/a.txt"), PathBuf::from("/tmp/x/z.txt"))]
        );
        assert_eq!(bulk.changes(), 1);
        assert_eq!(bulk.first_change(), Some("z.txt".to_string()));
    }

    /// A card over files from two folders judges each folder on its own: two
    /// `a.txt` rows may both become `b.txt`, a name is taken only by a file in
    /// its own folder, each rename stays in its folder, and the old-name
    /// column says which folder a row is from.
    #[test]
    fn a_card_across_folders_judges_each_folder_on_its_own() {
        let (one, two) = (PathBuf::from("/r/one"), PathBuf::from("/r/two"));
        let others = HashMap::from([
            (one.clone(), HashSet::from(["taken.txt".to_string()])),
            (two.clone(), HashSet::new()),
        ]);
        let mut bulk = Bulk::from_rows(
            PathBuf::from("/r"),
            vec![one.clone(), two.clone()],
            vec![file("a.txt", None), file("a.txt", None)],
            others,
        );
        // Labelled in the platform's separator, which the test reads as `/`.
        let label = |row| df_core::path::with_slashes(&bulk.label(row)).into_owned();
        assert_eq!(
            (label(0), label(1)),
            ("one/a.txt".into(), "two/a.txt".into())
        );

        run(&mut bulk, "<ctrl+u>b.txt");
        assert!(bulk.valid(), "{:?}", bulk.problems());
        assert_eq!(
            bulk.renames(),
            vec![
                (one.join("a.txt"), one.join("b.txt")),
                (two.join("a.txt"), two.join("b.txt"))
            ]
        );
        assert_eq!(bulk.first_changed(), Some(one.join("a.txt")));

        run(&mut bulk, "<ctrl+u>taken.txt");
        assert_eq!(bulk.problems(), vec![Some(Problem::Taken), None]);
    }
}
