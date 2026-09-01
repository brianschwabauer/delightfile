//! The bulk-rename diff view — PLAN §5's "in-app two-column editable diff
//! (old → new) with live validation, instead of shelling to an editor".
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
//! ## The three ways a name can be wrong
//!
//! They are different problems and they read differently, so they are three
//! variants and not one "invalid":
//!
//! - **Unusable** — empty, `.`/`..`, or containing a `/`. Nothing can be named
//!   this; the row is wrong on its own terms.
//! - **Duplicate** — two rows in *this card* want the same name. Neither row is
//!   wrong by itself, which is why both are marked: the fix is a choice between
//!   them.
//! - **Taken** — something already on disk has that name, and it is not one of
//!   the files being renamed. A file that is being renamed *away* does not
//!   count, which is what makes swapping two names (`a`→`b`, `b`→`a`) legal
//!   here even though doing it by hand needs a temporary.
//!
//! The swap case is also why the rename runs in two passes; see
//! [`ordered_renames`].
//!
//! ## Find and replace
//!
//! The field at the top rewrites every row that has not been touched by hand.
//! Hand-edited rows are left alone on purpose: the field is for the bulk of the
//! work and the rows are for the exceptions, and a replace that silently threw
//! away an exception you had just typed would make the two fight.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use df_core::input::InputBuffer;

/// How many rows the card shows at once before it scrolls.
///
/// Twelve: enough that a normal selection is on screen whole, few enough that
/// the card stays a card rather than becoming a second file pane. Past this the
/// list scrolls under the cursor by the same scrolloff rule the panes use.
pub const ROWS: usize = 12;

/// The longest name this card will accept, in bytes.
///
/// Linux's own `NAME_MAX`. Refusing at 255 with a readable message beats letting
/// the rename fail with `ENAMETOOLONG` half way through a batch.
pub const NAME_MAX: usize = 255;

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
}

impl Problem {
    /// The inline message, in the words a person can act on.
    pub fn message(self) -> &'static str {
        match self {
            Problem::Unusable => "not a usable name",
            Problem::Duplicate => "two rows want this name",
            Problem::Taken => "already exists here",
            Problem::TooLong => "too long",
        }
    }
}

/// Which field the keyboard is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The find/replace field at the top.
    Find,
    Replace,
    /// One of the name rows.
    Row(usize),
}

/// One line of the diff: the old name, and the new one being edited.
pub struct Row {
    pub old: String,
    pub buffer: InputBuffer,
    /// Whether this row has been typed into by hand, and so is exempt from
    /// find/replace. See the module essay.
    pub edited: bool,
}

impl Row {
    fn new(old: String) -> Row {
        let buffer = InputBuffer::for_rename_stem(&old);
        Row {
            old,
            buffer,
            edited: false,
        }
    }

    pub fn new_name(&self) -> &str {
        self.buffer.text()
    }

    /// Whether this row would actually do anything.
    pub fn changed(&self) -> bool {
        self.new_name() != self.old
    }
}

/// The card, while it is up.
pub struct Bulk {
    /// The directory every one of these names is in. One directory, always: a
    /// selection spans one listing, and a card that renamed across directories
    /// would be a move with a text field for a destination.
    pub dir: PathBuf,
    pub rows: Vec<Row>,
    pub find: InputBuffer,
    pub replace: InputBuffer,
    pub field: Field,
    /// The first row drawn, for the scroll.
    pub first: usize,
    /// Names already in the directory that are not part of this card — the
    /// "taken" test, snapshotted when the card opened rather than re-read per
    /// keystroke.
    others: HashSet<String>,
}

impl Bulk {
    /// Open the card over a selection.
    ///
    /// `siblings` is every name in the directory; the ones being renamed are
    /// removed from it here, so a row keeping its own name is not reported as
    /// colliding with itself.
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
    ) -> Result<Bulk, &'static str> {
        if crate::remote::is_remote(&dir) {
            return Err("Rename one at a time over the link — r renames the row under the cursor");
        }
        Ok(Bulk::local(dir, names, siblings))
    }

    /// The card itself, once the directory has been vouched for.
    fn local(dir: PathBuf, names: Vec<String>, siblings: &[String]) -> Bulk {
        let chosen: HashSet<&str> = names.iter().map(String::as_str).collect();
        let others = siblings
            .iter()
            .filter(|name| !chosen.contains(name.as_str()))
            .cloned()
            .collect();
        Bulk {
            dir,
            rows: names.into_iter().map(Row::new).collect(),
            find: InputBuffer::new(String::new(), 0),
            replace: InputBuffer::new(String::new(), 0),
            field: Field::Row(0),
            first: 0,
            others,
        }
    }

    /// The buffer the keyboard is typing into.
    pub fn buffer_mut(&mut self) -> Option<&mut InputBuffer> {
        match self.field {
            Field::Find => Some(&mut self.find),
            Field::Replace => Some(&mut self.replace),
            Field::Row(index) => self.rows.get_mut(index).map(|row| &mut row.buffer),
        }
    }

    /// Move between fields: the two at the top, then the rows, wrapping.
    ///
    /// Wrapping, like the palette and unlike the file panes: this is a short
    /// ranked-by-position menu of fields you are stepping around, not a place
    /// with a top and a bottom to lose your position in.
    pub fn step(&mut self, delta: isize) {
        let fields = self.rows.len() + 2;
        let at = match self.field {
            Field::Find => 0isize,
            Field::Replace => 1,
            Field::Row(index) => index as isize + 2,
        };
        let next = (at + delta).rem_euclid(fields as isize) as usize;
        self.field = match next {
            0 => Field::Find,
            1 => Field::Replace,
            n => Field::Row(n - 2),
        };
        self.scroll_into_view();
    }

    /// Keep the edited row on screen, by the panes' own scrolloff rule.
    pub fn scroll_into_view(&mut self) {
        let Field::Row(index) = self.field else {
            // Editing the find field shows the *top* of the list, because that
            // is where the eye goes to check what the replace did.
            self.first = 0;
            return;
        };
        self.first = crate::viewport::first_visible(self.first, index, self.rows.len(), ROWS, 2);
    }

    /// Mark the row the keyboard is in as hand-edited, so find/replace leaves
    /// it alone from now on.
    pub fn touched(&mut self) {
        if let Field::Row(index) = self.field {
            if let Some(row) = self.rows.get_mut(index) {
                row.edited = true;
            }
        }
    }

    /// Re-run find/replace over every row that has not been hand-edited.
    ///
    /// Called after a keystroke in either of the top two fields. An empty find
    /// puts the original names back, which is what makes the field undoable by
    /// deleting it.
    pub fn apply_replace(&mut self) {
        let find = self.find.text().to_string();
        let with = self.replace.text().to_string();
        for row in &mut self.rows {
            if row.edited {
                continue;
            }
            let next = replaced(&row.old, &find, &with);
            if row.buffer.text() != next {
                // A fresh buffer rather than an edit: the caret belongs at the
                // stem of the *new* name, and carrying the old caret over would
                // put it inside a word that is no longer there.
                row.buffer = InputBuffer::for_rename_stem(&next);
            }
        }
    }

    /// What is wrong with each row, in row order.
    pub fn problems(&self) -> Vec<Option<Problem>> {
        let names: Vec<&str> = self.rows.iter().map(Row::new_name).collect();
        problems(&names, &self.others)
    }

    /// Whether `Enter` is allowed.
    pub fn valid(&self) -> bool {
        self.problems().iter().all(Option::is_none)
    }

    /// How many rows would actually change.
    pub fn changes(&self) -> usize {
        self.rows.iter().filter(|row| row.changed()).count()
    }

    /// The renames to carry out, in an order that is safe to run one at a time.
    pub fn renames(&self) -> Vec<(PathBuf, PathBuf)> {
        let pairs: Vec<(String, String)> = self
            .rows
            .iter()
            .filter(|row| row.changed())
            .map(|row| (row.old.clone(), row.new_name().to_string()))
            .collect();
        ordered_renames(&self.dir, &pairs)
    }
}

/// `find` → `with` over `text`, or `text` unchanged when `find` is empty.
///
/// Plain substring replacement, every occurrence. Not a regex: the field is
/// there so that renaming forty `IMG_` files to `holiday_` is one gesture, and
/// a regex engine would be a dependency and a syntax to learn for a job that is
/// almost always a literal.
pub fn replaced(text: &str, find: &str, with: &str) -> String {
    if find.is_empty() {
        return text.to_string();
    }
    text.replace(find, with)
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
        );
        let why = refused
            .err()
            .expect("a remote directory has no bulk rename");
        assert!(why.contains('r'), "the notice names the way out: {why}");

        // A local directory is unaffected, and the card it builds is the one
        // the rest of these tests exercise.
        let ok = Bulk::new(PathBuf::from("/tmp/somewhere"), names, &[]).expect("a local card");
        assert_eq!(ok.rows.len(), 2);
    }

    fn card(names: &[&str], siblings: &[&str]) -> Bulk {
        Bulk::local(
            PathBuf::from("/tmp/x"),
            names.iter().map(|s| s.to_string()).collect(),
            &siblings.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// The three ways a name can be wrong, each reported as itself.
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

    /// Find/replace rewrites the untouched rows and leaves the hand-edited one
    /// alone.
    #[test]
    fn replace_rewrites_every_row_it_has_not_been_told_to_leave() {
        let mut bulk = card(&["IMG_1.jpg", "IMG_2.jpg", "IMG_3.jpg"], &[]);
        // Row 1 is edited by hand before the replace runs.
        bulk.field = Field::Row(1);
        bulk.touched();
        bulk.rows[1].buffer = InputBuffer::new("keeper.jpg".to_string(), 0);

        bulk.find = InputBuffer::new("IMG_".to_string(), 0);
        bulk.replace = InputBuffer::new("holiday_".to_string(), 0);
        bulk.apply_replace();

        assert_eq!(bulk.rows[0].new_name(), "holiday_1.jpg");
        assert_eq!(bulk.rows[1].new_name(), "keeper.jpg", "hand edits survive");
        assert_eq!(bulk.rows[2].new_name(), "holiday_3.jpg");
        assert_eq!(bulk.changes(), 3);
        assert!(bulk.valid());

        // Deleting the find text puts the untouched rows back — the field is
        // undoable by emptying it.
        bulk.find = InputBuffer::new(String::new(), 0);
        bulk.apply_replace();
        assert_eq!(bulk.rows[0].new_name(), "IMG_1.jpg");
        assert_eq!(bulk.rows[1].new_name(), "keeper.jpg");
    }

    #[test]
    fn replacing_nothing_changes_nothing() {
        assert_eq!(replaced("a.txt", "", "z"), "a.txt");
        assert_eq!(replaced("aa.txt", "a", "b"), "bb.txt");
        assert_eq!(replaced("a.txt", "zzz", "b"), "a.txt");
    }

    /// The keyboard walks find → replace → every row → back to find.
    #[test]
    fn the_fields_step_in_order_and_wrap() {
        let mut bulk = card(&["a", "b"], &[]);
        bulk.field = Field::Find;
        bulk.step(1);
        assert_eq!(bulk.field, Field::Replace);
        bulk.step(1);
        assert_eq!(bulk.field, Field::Row(0));
        bulk.step(1);
        assert_eq!(bulk.field, Field::Row(1));
        bulk.step(1);
        assert_eq!(bulk.field, Field::Find, "wraps at the end");
        bulk.step(-1);
        assert_eq!(bulk.field, Field::Row(1), "and at the start");
    }

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

    /// Rows that did not change are not renamed at all.
    #[test]
    fn unchanged_rows_produce_no_work() {
        let mut bulk = card(&["a.txt", "b.txt"], &[]);
        bulk.rows[0].buffer = InputBuffer::new("z.txt".to_string(), 0);
        let renames = bulk.renames();
        assert_eq!(
            renames,
            vec![(PathBuf::from("/tmp/x/a.txt"), PathBuf::from("/tmp/x/z.txt"))]
        );
        assert_eq!(bulk.changes(), 1);
    }
}
