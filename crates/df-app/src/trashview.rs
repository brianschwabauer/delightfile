//! The trash, browsed as a directory (PLAN §7.4's "a virtual trash:// location
//! to browse/restore").
//!
//! ## The same trick again
//!
//! `g t` does not open a *trash manager*; it puts a different set of rows in
//! the list pane. The rows are [`df_core::fs::Entry`] values built from
//! [`df_core::ops::TrashedItem`]s and handed to the pane's `DirState` through
//! [`DirState::set_entries`](df_core::fs::DirState::set_entries) — the archive
//! and remote pattern for the third time — so the cursor, `Space`, `Ctrl+a`,
//! visual mode, the filter, the sorts, the icons and the preview pane all work
//! on trashed files without knowing they are trashed.
//!
//! ## The paths are real, which is what makes the preview work
//!
//! A row's [`df_core::fs::Entry::path`] is its file inside `…/Trash/files/`, which genuinely
//! exists. So the ordinary preview pipeline opens it, the ordinary icon table
//! recognises it, and `Tab` shows real metadata — none of which needed a line
//! of new code. The row's *name* is the name inside `files/`, not the original
//! one, for a reason worth stating: trashing `notes.txt` twice from two
//! directories leaves `notes.txt` and `notes_1.txt` in the trash, and the pane
//! keys selection, the cursor and the filter by name. Two rows called
//! `notes.txt` would be one row as far as `Space` was concerned. The original
//! name is one segment of the original path, which the linemode column shows on
//! every row anyway.
//!
//! ## What the columns mean here
//!
//! - The **linemode column is the original directory**, always, whatever `m`
//!   says. That is the fact a person is in the trash to read — "which of these
//!   two `Cargo.toml`s is mine" — and the size or the owner of a file that is
//!   already deleted answers nothing.
//! - **`mtime` is the deletion date**, so `, m` sorts by when things were
//!   thrown away and the newest mistake is one keystroke from the top. It is
//!   the sort the view opens on.
//!
//! ## The three verbs
//!
//! `Enter`/`r` restore, `D` purges, and "Empty trash" purges everything. They
//! reuse the keys they already have rather than inventing a trash keymap, and
//! all three go through [`df_core::ops::trash`] — restore is the journal's own
//! machinery (`u` after a `d` calls exactly [`df_core::ops::trash::restore`]),
//! so the two doors to it cannot drift.
//!
//! ### What is journalled, and what is not
//!
//! Neither verb records anything, and both omissions are deliberate.
//!
//! A **restore** is already an undo — it is the exact operation `u` performs
//! against an `OpRecord::Trash` — and its own inverse is "trash it again",
//! which is `d` on the file that just came back. Recording it would mean a
//! `Restore` variant in [`df_core::ops::journal::OpRecord`] whose replay is a
//! keystroke away, and an `u` stack in which undo and redo of the same gesture
//! alternate forever.
//!
//! A **purge** has no inverse to record. It is the second of the program's two
//! irreversible operations, and like `D` it is journalled nowhere because a
//! journal entry that cannot be replayed is a promise the program cannot keep
//! (PLAN §5). What it *can* leave behind is a stale `OpRecord::Trash` on the
//! undo stack — `d` something, purge it, press `u`. That is not a hole: the
//! restore refuses with "…is no longer in the trash", which is both true and
//! the sentence somebody needs.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use df_core::du::{DuMessage, DuToken};
use df_core::fs::{mime, Entry, Kind};
use df_core::ops::trash::Purged;
use df_core::ops::TrashedItem;

use crate::folders::Size;

/// The path the pane's `DirState` is called while the trash is on screen.
///
/// A URL, in the same spirit as `sftp://…`: it is not a directory, it must
/// never be handed to the filesystem, and it is recognisable at a glance in a
/// log line. The real `…/Trash` directory is not used as the pane's path
/// because that would make `←` walk into `~/.local/share`, which is not where
/// the user came from.
pub const URL: &str = "trash://";

/// What the breadcrumb says.
pub const LABEL: &str = "Trash";

/// The trash the list pane is showing.
pub struct View {
    /// Every item, in the order the rows were built from — the row for a name
    /// is found by name, so this does not have to stay parallel with the
    /// pane's sorted view.
    pub items: Vec<TrashedItem>,
    /// The local directory `←` returns to. Remembered rather than assumed,
    /// for the same reason a remote session remembers one: a jump you can take
    /// back is a jump people will make.
    pub origin: PathBuf,
}

impl View {
    /// The item a row belongs to. By name, which is unique inside one trash's
    /// `files/` by construction — see the module note.
    pub fn item(&self, name: &OsStr) -> Option<&TrashedItem> {
        // `OsStr`, not a lossy `String`: two different names that are both
        // invalid UTF-8 can render as the same replacement characters, and
        // `find` would then hand a purge the wrong item.
        self.items.iter().find(|i| i.name == name)
    }

    /// The items a set of row paths names, in the pane's order.
    pub fn items_for(&self, paths: &[PathBuf]) -> Vec<TrashedItem> {
        paths
            .iter()
            .filter_map(|path| path.file_name())
            .filter_map(|name| self.item(name))
            .cloned()
            .collect()
    }
}

/// The breadcrumb: one chip that says where you are, and nothing to click into.
pub fn crumbs() -> Vec<crate::chrome::Crumb> {
    vec![crate::chrome::Crumb {
        label: LABEL.to_string(),
        path: PathBuf::from(URL),
        accent: true,
    }]
}

/// Every item as a list-pane row.
pub fn rows(items: &[TrashedItem]) -> Vec<Entry> {
    items.iter().map(row).collect()
}

/// One item, as a row.
///
/// The metadata comes from the file itself when it is readable, so a trashed
/// directory sorts and draws as a directory and a trashed file carries its real
/// size. A file that cannot be stat'd — a trash on a mount that went away —
/// still gets a row, because a row you can see and fail to restore beats an
/// item that silently is not listed.
pub fn row(item: &TrashedItem) -> Entry {
    let path = item.files_path();
    let meta = std::fs::symlink_metadata(&path).ok();
    // The *second* stat, following the link. Only asked for when there is a
    // link to follow, so an ordinary trash listing is one `lstat` a row as it
    // always was.
    let target = meta
        .as_ref()
        .is_some_and(|m| m.file_type().is_symlink())
        .then(|| std::fs::metadata(&path).ok())
        .flatten();
    row_from(item, meta.as_ref(), target.as_ref())
}

/// What a trashed symlink resolves to, from the stat that followed it. `None`
/// is a broken link, which in a trash is the common case: the target usually
/// went into the trash with it, under a different name.
fn link_target(target: Option<&std::fs::Metadata>) -> Option<df_core::fs::LinkTarget> {
    use df_core::fs::LinkTarget;
    let target = target?;
    Some(if target.is_dir() {
        LinkTarget::Dir
    } else if target.is_file() {
        LinkTarget::File
    } else {
        LinkTarget::Other
    })
}

/// The pure half, so the mapping is a table test rather than something you have
/// to delete a file to check.
///
/// `meta` is the `lstat` — the link itself — and `target` the stat through it,
/// when there is one.
pub fn row_from(
    item: &TrashedItem,
    meta: Option<&std::fs::Metadata>,
    target: Option<&std::fs::Metadata>,
) -> Entry {
    use std::os::unix::fs::MetadataExt;
    let name = item.name.to_string_lossy().into_owned();
    let original_name = item
        .original
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.clone());
    // **A trashed symlink is a symlink, and its facts are the target's.**
    // `meta` is an `lstat`: its mode carries `S_IFLNK` and its `len` is the
    // length of the path the link holds. Handing those to `classify` alongside
    // `Kind::File` made every trashed link a `Special`, drawn with the
    // socket-or-device glyph — the file-type bits are exactly what that check
    // reads. Both halves are stated properly instead, which is the rule an
    // ordinary listing already follows (`df_core::fs::Entry::from_parts`): the
    // kind says what the link resolved to, and the stat the row is built from
    // is the one taken *through* it.
    let is_link = meta.is_some_and(|m| m.file_type().is_symlink());
    let kind = if is_link {
        Kind::Symlink {
            target: link_target(target),
        }
    } else if meta.is_some_and(|m| m.is_dir()) {
        Kind::Dir
    } else {
        Kind::File
    };
    // A broken link keeps its own metadata: its mtime is when the link was
    // made, which is the only true thing left to show.
    let facts = if is_link { target.or(meta) } else { meta };
    let is_dir = matches!(
        kind,
        Kind::Dir
            | Kind::Symlink {
                target: Some(df_core::fs::LinkTarget::Dir)
            }
    );
    let mode = facts
        .map(|m| m.mode())
        .unwrap_or(if is_dir { 0o040_755 } else { 0o100_644 });
    // Sniffed from the *original* name, so a `notes_1.txt` in the trash still
    // gets the icon and the preview of the `notes.txt` it was.
    let mime = if is_dir {
        mime::DIR_MIME
    } else {
        mime::hint_for_name(&original_name)
    };
    Entry {
        // A dotfile is hidden in the trash too: `.` still means "show me the
        // ones I normally do not look at", and a trash full of `.cache`
        // fragments is exactly what that key is for.
        is_hidden: original_name.starts_with('.'),
        path: item.files_path(),
        kind,
        len: if is_dir {
            0
        } else {
            facts.map(|m| m.len()).unwrap_or(0)
        },
        // **The deletion date, not the file's own mtime.** It is the date the
        // trash is read for, and putting it here means `, m` sorts by it and
        // the mtime linemode shows it without a second column existing.
        mtime: deleted_at(&item.deleted_at),
        btime: None,
        mode,
        uid: facts.map(|m| m.uid()).unwrap_or(0),
        gid: facts.map(|m| m.gid()).unwrap_or(0),
        mime,
        // …and the kind follows the original name for the same reason.
        file_kind: df_core::fs::classify(kind, &original_name, mime, mode),
        name,
    }
}

/// What the linemode column shows for each row: the directory the file came
/// out of, keyed by the row's name.
///
/// The directory rather than the whole path, because the name is already the
/// first column and repeating it in the second would waste the only space
/// there is for the part that distinguishes two rows.
pub fn notes(items: &[TrashedItem]) -> std::collections::HashMap<String, String> {
    items
        .iter()
        .map(|item| {
            // An orphan has no origin to name; the column says what is wrong
            // with the row instead, which is the fact a person is in the trash
            // to read about it.
            if item.is_orphan() {
                return (
                    item.name.to_string_lossy().into_owned(),
                    "no record — D destroys it".to_string(),
                );
            }
            let dir = item
                .original
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| item.original.to_string_lossy().into_owned());
            (item.name.to_string_lossy().into_owned(), shorten(&dir))
        })
        .collect()
}

/// `$HOME/Work/x` → `~/Work/x`. The column is narrow and the home prefix is the
/// part every row shares, so it is the part worth spending one character on.
fn shorten(path: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_string();
    };
    let home = home.to_string_lossy().into_owned();
    if home.is_empty() {
        return path.to_string();
    }
    match path.strip_prefix(&home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// `YYYY-MM-DDThh:mm:ss` (UTC, as [`df_core::ops::trash`] writes it) as a
/// [`SystemTime`].
///
/// df-core's parser ([`df_core::ops::trash::parse_deletion_date`]), because the
/// automatic purge reads the same field to decide what is old, and the date a
/// row shows and the date that decides whether it is destroyed must be one
/// reading. Anything that does not parse comes back `None`, which the mtime
/// linemode and the mtime sort already tolerate — an item with an unreadable
/// date is still listed and still restorable, which is the only thing that
/// matters.
pub fn deleted_at(text: &str) -> Option<SystemTime> {
    df_core::ops::trash::parse_deletion_date(text)
}

/// Why a restore was refused, in the words the toast shows.
///
/// The refusals are df-core's — [`df_core::ops::trash::restore`] is the one
/// implementation, and it is the same one `u` runs — but the *check* is worth
/// having here as well, because a restore of forty items should tell the user
/// which ones will fail before it starts moving the other thirty-nine, and
/// because "the original name is taken again" is a sentence somebody can act on
/// (PLAN §5: undo may never overwrite newer work).
pub fn restore_refusal(item: &TrashedItem) -> Option<String> {
    // A file in `files/` that no record describes — what a cancelled purge
    // leaves behind (see [`df_core::ops::trash::purge`]). There is nowhere to
    // put it back, and the row says so rather than failing with a sentence
    // about a path that is the empty string.
    if item.is_orphan() {
        return Some(format!(
            "{} has no record in the trash — D destroys it",
            item.name.to_string_lossy()
        ));
    }
    let original = &item.original;
    // `symlink_metadata`, matching df-core's `exists`: a trashed *broken*
    // symlink is a thing this program supports trashing, and `Path::exists`
    // follows the link and answers "gone" — which refused a restore that
    // df-core would have carried out perfectly.
    if std::fs::symlink_metadata(item.files_path()).is_err() {
        return Some(format!("{} is no longer in the trash", original.display()));
    }
    if std::fs::symlink_metadata(original).is_ok() {
        return Some(format!(
            "{} exists again — rename it and restore afterwards",
            original.display()
        ));
    }
    match original.parent() {
        Some(parent) if std::fs::symlink_metadata(parent).is_err() => Some(format!(
            "{} is gone, so {} cannot go back into it",
            parent.display(),
            name_of(original)
        )),
        Some(_) => None,
        None => Some(format!("{} has no parent", original.display())),
    }
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

// ── What the trash weighs, and how long it keeps things ─────────────────────

/// What the trash weighs, for the chip beside the position counter and the
/// Empty trash card.
///
/// The du scanner's walk of the trash's `files/` directory, in the background,
/// asked for again every time the view's rows change — the same walk, the same
/// niced workers and the same `~` the size column has. The last answer is kept
/// across walks: a re-walk after a restore shows the old number wearing its `~`
/// until the new count passes it or settles, rather than counting up from
/// nothing each time a row goes.
#[derive(Debug, Default)]
pub struct Weight {
    /// The walk being read, while there is one.
    token: Option<DuToken>,
    /// What is known so far.
    size: Option<Size>,
}

impl Weight {
    /// A walk has been asked for. What was known stays, unsettled.
    pub fn begin(&mut self, token: DuToken) {
        self.token = Some(token);
        if let Some(size) = &mut self.size {
            size.settled = false;
        }
    }

    /// The walk being read, if any.
    pub fn token(&self) -> Option<DuToken> {
        self.token
    }

    /// Stop reading the walk. Returns its token, so the scanner can be told to
    /// stop it too.
    pub fn stop(&mut self) -> Option<DuToken> {
        self.token.take()
    }

    /// What is known, if anything.
    pub fn size(&self) -> Option<Size> {
        self.size
    }

    /// Take one message from the walk. Returns whether the number moved.
    ///
    /// Only the root's own total is read — nothing deeper is asked for — and
    /// a running total only replaces a larger number when it is the final one,
    /// so the `~` keeps meaning what it means in the size column: still
    /// counting, and only going up.
    pub fn apply(&mut self, message: DuMessage) -> bool {
        if Some(message.token()) != self.token {
            return false;
        }
        match message {
            DuMessage::Progress { updates, .. } => {
                let mut moved = false;
                for update in updates.iter().filter(|update| update.depth == 0) {
                    let known = self.size.map(|size| size.bytes);
                    if known.is_none_or(|bytes| update.total_bytes > bytes) {
                        self.size = Some(Size {
                            bytes: update.total_bytes,
                            settled: false,
                        });
                        moved = true;
                    }
                }
                moved
            }
            DuMessage::Done { totals, .. } => {
                self.token = None;
                self.size = Some(Size {
                    bytes: totals.total_bytes,
                    settled: true,
                });
                true
            }
            // A `files/` that cannot be walked leaves the count to speak for
            // itself, rather than a number nobody could have counted.
            DuMessage::Failed { .. } => {
                self.token = None;
                self.size = None;
                true
            }
            DuMessage::Started { .. } | DuMessage::Counts { .. } => false,
        }
    }
}

/// `37 items · 1.2 GB` — or `37 items · ~1.2 GB` while the walk is counting,
/// or `37 items` before it has said anything. The chip's words, and the middle
/// of the Empty trash card's question.
///
/// The bytes in the size column's own words ([`crate::format::folder_size_text`])
/// so a `~` here means what a `~` there does.
pub fn weight_text(count: usize, size: Option<Size>) -> String {
    let items = format!(
        "{} {}",
        df_core::text::grouped(count as u64),
        if count == 1 { "item" } else { "items" }
    );
    match crate::format::folder_size_text(size, None) {
        Some(bytes) => format!("{items} · {bytes}"),
        None => items,
    }
}

/// What the trash says about its own clock — or nothing, when it has none
/// (`[mgr] trash_keep_days = 0`).
pub fn keep_text(keep_days: u64) -> Option<String> {
    (keep_days > 0).then(|| format!("Items are removed for good after {}", days(keep_days)))
}

/// `30 days`, `1 day`.
pub fn days(n: u64) -> String {
    if n == 1 {
        "1 day".to_string()
    } else {
        format!("{} days", df_core::text::grouped(n))
    }
}

/// What an automatic purge says when it is over: the words, and whether they
/// are an error — or `None` when there is nothing to say, which is most days:
/// nothing was old enough.
///
/// Items that would not go are the error, counted and with the first reason,
/// and the same words are the task's failure in `w`.
pub fn purged_text(report: &Purged, keep_days: u64) -> Option<(String, bool)> {
    let count = |n: usize, what: &str| match n {
        1 => format!("1 {what}"),
        n => format!("{} {what}s", df_core::text::grouped(n as u64)),
    };
    let emptied = format!(
        "Emptied {} older than {} from the trash",
        count(report.removed, "item"),
        days(keep_days)
    );
    if report.failed == 0 {
        return (report.removed > 0).then_some((emptied, false));
    }
    let stuck = format!(
        "{} could not be removed: {}",
        count(report.failed, "old item"),
        report.first_error.as_deref().unwrap_or("no reason given")
    );
    Some((
        if report.removed > 0 {
            format!("{emptied} — {stuck}")
        } else {
            stuck
        },
        true,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::time::{Duration, UNIX_EPOCH};

    fn item(name: &str, original: &str, deleted_at: &str) -> TrashedItem {
        TrashedItem {
            trash_root: PathBuf::from("/home/brian/.local/share/Trash"),
            name: OsString::from(name),
            original: PathBuf::from(original),
            deleted_at: deleted_at.to_string(),
        }
    }

    /// The mapping from a trash record to a row, pinned: the in-trash name, the
    /// real path, and the deletion date standing in for the modification time.
    #[test]
    fn a_trash_record_becomes_a_list_pane_row() {
        let i = item(
            "notes_1.txt",
            "/home/brian/Work/notes.txt",
            "2026-08-30T09:15:00",
        );
        let row = row_from(&i, None, None);
        assert_eq!(row.name, "notes_1.txt");
        assert_eq!(
            row.path,
            PathBuf::from("/home/brian/.local/share/Trash/files/notes_1.txt")
        );
        assert!(!row.is_dir());
        // The icon and the preview follow the *original* name, so a collision
        // suffix does not turn a text file into an unknown one.
        assert_eq!(row.mime, "text/plain");
        // No archive-style creation time invented from thin air.
        assert!(row.btime.is_none());
        // The deletion date is the row's date, which is what `, m` sorts by.
        assert_eq!(row.mtime, deleted_at("2026-08-30T09:15:00"));
        assert!(row.mtime.is_some());
    }

    /// A trashed symlink is a symlink, not a device node.
    ///
    /// The row is built from an `lstat`, whose mode carries `S_IFLNK` — and
    /// `classify` reads the file-type bits to tell a socket from a file. Handed
    /// those bits with `Kind::File`, it called every trashed link `Special` and
    /// the icon column drew the device glyph for a shortcut to a text file.
    #[test]
    fn a_trashed_symlink_is_a_symlink_and_not_a_special_file() {
        use df_core::fs::{FileKind, Kind, LinkTarget};

        let tree = df_core::test_support::TempTree::new("trashview-symlink");
        let target = tree.path().join("notes.txt");
        std::fs::write(&target, b"hello").expect("target");
        let link = tree.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let i = item("link", "/home/brian/Work/link", "2026-08-30T09:15:00");
        let lstat = std::fs::symlink_metadata(&link).expect("lstat");
        let stat = std::fs::metadata(&link).expect("stat");

        let row = row_from(&i, Some(&lstat), Some(&stat));
        assert_eq!(
            row.kind,
            Kind::Symlink {
                target: Some(LinkTarget::File)
            }
        );
        assert_ne!(row.file_kind, FileKind::Special, "the bug this pins");
        assert!(row.is_symlink() && !row.is_dir());

        // A link whose target went into the trash under another name is broken,
        // and says so rather than becoming a device node by a different route.
        let broken = row_from(&i, Some(&lstat), None);
        assert_eq!(broken.kind, Kind::Symlink { target: None });
        assert_eq!(broken.file_kind, FileKind::BrokenLink);

        // …and a link to a directory reads as one, the same as it does in an
        // ordinary listing.
        let dir = tree.path().join("sub");
        std::fs::create_dir(&dir).expect("dir");
        let dir_stat = std::fs::metadata(&dir).expect("stat");
        let to_dir = row_from(&i, Some(&lstat), Some(&dir_stat));
        assert_eq!(to_dir.file_kind, FileKind::Directory);
        assert!(to_dir.is_dir());
    }

    /// A dotfile stays hidden in the trash, judged by the name it had — the
    /// in-trash name of `.bashrc` deleted twice is still `.bashrc_1`, but the
    /// rule must not depend on that.
    #[test]
    fn hidden_files_are_still_hidden_in_the_trash() {
        assert!(
            row_from(
                &item("x", "/home/brian/.bashrc", "2026-08-30T09:15:00"),
                None,
                None
            )
            .is_hidden
        );
        assert!(
            !row_from(
                &item("x", "/home/brian/notes.txt", "2026-08-30T09:15:00"),
                None,
                None
            )
            .is_hidden
        );
    }

    /// Newest first is the sort the view opens on, and it falls out of the
    /// deletion date being the row's date rather than needing a sort of its own.
    #[test]
    fn rows_sort_by_when_they_were_thrown_away() {
        let items = vec![
            item("old.txt", "/home/brian/old.txt", "2024-01-02T03:04:05"),
            item("new.txt", "/home/brian/new.txt", "2026-08-31T23:59:59"),
            item("mid.txt", "/home/brian/mid.txt", "2025-06-15T12:00:00"),
        ];
        let mut rows = rows(&items);
        rows.sort_by_key(|row| std::cmp::Reverse(row.mtime));
        let order: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(order, vec!["new.txt", "mid.txt", "old.txt"]);
    }

    /// The `.trashinfo` date format, both directions, against the writer that
    /// produced it — so a change to either side breaks a test rather than a
    /// column.
    #[test]
    fn the_deletion_date_round_trips_through_the_spec_format() {
        for stamp in [0u64, 1, 951_827_696, 1_756_598_400, 4_102_444_800] {
            let t = UNIX_EPOCH + Duration::from_secs(stamp);
            let text = df_core::ops::trash::iso8601_utc(t);
            assert_eq!(deleted_at(&text), Some(t), "{text}");
        }
        // Anything that is not the format is a hole, not a panic: an item with
        // an unreadable date is still listed and still restorable.
        assert_eq!(deleted_at(""), None);
        assert_eq!(deleted_at("yesterday"), None);
        assert_eq!(deleted_at("2026-13-01T00:00:00"), None);
        assert_eq!(deleted_at("2026-08-31T25:00:00"), None);
        assert_eq!(deleted_at("2026/08/31T00:00:00"), None);
    }

    /// The linemode column: the original directory, with `$HOME` folded to `~`.
    #[test]
    fn the_column_shows_where_each_row_came_from() {
        let home = std::env::var("HOME").unwrap_or_default();
        let items = vec![
            item(
                "a.txt",
                &format!("{home}/Work/a.txt"),
                "2026-08-30T09:15:00",
            ),
            item("b.txt", "/etc/b.txt", "2026-08-30T09:15:00"),
        ];
        let notes = notes(&items);
        if !home.is_empty() {
            assert_eq!(notes.get("a.txt").map(String::as_str), Some("~/Work"));
        }
        assert_eq!(notes.get("b.txt").map(String::as_str), Some("/etc"));
    }

    /// Every path a restore can be refused on, against a real tree — including
    /// the one PLAN §5 exists for: the original name being taken again.
    #[test]
    fn a_restore_is_refused_rather_than_overwriting_newer_work() {
        let tree = df_core::test_support::TempTree::new("trashview-restore");
        let root = tree.path();
        let trash = root.join("Trash");
        std::fs::create_dir_all(trash.join("files")).expect("files");
        std::fs::create_dir_all(trash.join("info")).expect("info");
        std::fs::create_dir_all(root.join("work")).expect("work");

        let make = |name: &str, original: PathBuf| {
            std::fs::write(trash.join("files").join(name), b"body").expect("write");
            TrashedItem {
                trash_root: trash.clone(),
                name: OsString::from(name),
                original,
                deleted_at: "2026-08-30T09:15:00".to_string(),
            }
        };

        // The happy path: nothing in the way, so nothing is refused.
        let fine = make("fine.txt", root.join("work/fine.txt"));
        assert_eq!(restore_refusal(&fine), None);

        // The name came back while the file was in the trash. Undo may never
        // overwrite newer work, so this is a refusal with a way out in it.
        let taken = make("taken.txt", root.join("work/taken.txt"));
        std::fs::write(root.join("work/taken.txt"), b"newer").expect("write");
        let refusal = restore_refusal(&taken).expect("refused");
        assert!(refusal.contains("exists again"), "{refusal}");
        assert!(refusal.contains("rename"), "{refusal}");

        // The directory it came out of is gone.
        let orphan = make("orphan.txt", root.join("vanished/orphan.txt"));
        let refusal = restore_refusal(&orphan).expect("refused");
        assert!(refusal.contains("vanished"), "{refusal}");

        // The record outlived its file — an interrupted purge, or somebody
        // emptying the trash from another program.
        let ghost = make("ghost.txt", root.join("work/ghost.txt"));
        std::fs::remove_file(trash.join("files/ghost.txt")).expect("remove");
        let refusal = restore_refusal(&ghost).expect("refused");
        assert!(refusal.contains("no longer in the trash"), "{refusal}");

        // …and the refusal agrees with df-core's, which is the one that
        // actually runs. Two doors, one rule.
        let ctx = df_core::tasks::TaskCtx::detached();
        assert!(df_core::ops::trash::restore(&taken, &ctx).is_err());
        assert!(df_core::ops::trash::restore(&fine, &ctx).is_ok());
        assert!(root.join("work/fine.txt").exists());
    }

    /// A row is found by the name the pane keys everything else by, and a set
    /// of selected paths comes back as the items they name.
    #[test]
    fn rows_map_back_to_the_records_they_came_from() {
        let view = View {
            items: vec![
                item("a.txt", "/home/brian/a.txt", "2026-08-30T09:15:00"),
                item("a_1.txt", "/etc/a.txt", "2026-08-31T09:15:00"),
            ],
            origin: PathBuf::from("/home/brian"),
        };
        assert_eq!(
            view.item(OsStr::new("a_1.txt")).map(|i| i.original.clone()),
            Some(PathBuf::from("/etc/a.txt"))
        );
        assert!(view.item(OsStr::new("nope.txt")).is_none());
        let rows = rows(&view.items);
        let picked: Vec<PathBuf> = rows.iter().map(|r| r.path.clone()).collect();
        let items = view.items_for(&picked);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].original, PathBuf::from("/home/brian/a.txt"));
    }

    /// The chip's words: the count alone before the walk has said anything,
    /// then the bytes wearing the size column's `~` until they settle.
    #[test]
    fn the_weight_reads_like_the_size_column() {
        let gb = 1_288_490_189; // 1.2 GB, in the size column's 1024s
        assert_eq!(weight_text(37, None), "37 items");
        assert_eq!(
            weight_text(
                37,
                Some(Size {
                    bytes: gb,
                    settled: false
                })
            ),
            "37 items · ~1.2 GB"
        );
        assert_eq!(
            weight_text(
                1_234,
                Some(Size {
                    bytes: gb,
                    settled: true
                })
            ),
            "1,234 items · 1.2 GB"
        );
        assert_eq!(
            weight_text(
                1,
                Some(Size {
                    bytes: 512,
                    settled: true
                })
            ),
            "1 item · 512 B"
        );
    }

    /// A re-walk keeps the last number up, unsettled, until the new count
    /// passes it or the walk settles — and a walk that cannot be done leaves
    /// the count to speak for itself.
    #[test]
    fn a_weight_only_counts_up_until_it_settles() {
        use df_core::du::{DuTotals, DuUpdate};
        let root = PathBuf::from("/t/Trash/files");
        let progress = |token: DuToken, bytes: u64| DuMessage::Progress {
            token,
            root: root.clone(),
            updates: vec![DuUpdate {
                dir: root.clone(),
                depth: 0,
                total_bytes: bytes,
                apparent_bytes: bytes,
                files: 1,
                dirs: 1,
                done: false,
                entries: 1,
            }],
        };
        let done = |token: DuToken, bytes: u64| DuMessage::Done {
            token,
            root: root.clone(),
            totals: DuTotals {
                total_bytes: bytes,
                apparent_bytes: bytes,
                files: 1,
                dirs: 1,
            },
        };
        let size = |bytes, settled| Some(Size { bytes, settled });

        let mut weight = Weight::default();
        let first = DuToken(1);
        weight.begin(first);
        assert!(weight.apply(progress(first, 100)));
        assert_eq!(weight.size(), size(100, false));
        assert!(weight.apply(done(first, 4096)));
        assert_eq!(weight.size(), size(4096, true));
        assert_eq!(weight.token(), None, "a settled walk is not read any more");

        // Again, after a restore: the old number stays until it is passed.
        let second = DuToken(2);
        weight.begin(second);
        assert_eq!(weight.size(), size(4096, false));
        assert!(!weight.apply(progress(second, 10)));
        assert_eq!(weight.size(), size(4096, false));
        // A message from the walk that was replaced changes nothing.
        assert!(!weight.apply(done(first, 1)));
        assert!(weight.apply(done(second, 1024)));
        assert_eq!(weight.size(), size(1024, true));

        let third = DuToken(3);
        weight.begin(third);
        assert!(weight.apply(DuMessage::Failed {
            token: third,
            root: root.clone(),
            error: df_core::DfError::Op("gone".to_string()),
        }));
        assert_eq!(weight.size(), None);
        assert_eq!(weight_text(2, weight.size()), "2 items");
    }

    /// What the trash says about its clock, and what the purge says when it
    /// is over — nothing at all on the days nothing was old enough.
    #[test]
    fn the_clock_says_how_long_and_the_purge_says_how_many() {
        assert_eq!(
            keep_text(30).as_deref(),
            Some("Items are removed for good after 30 days")
        );
        assert_eq!(
            keep_text(1).as_deref(),
            Some("Items are removed for good after 1 day")
        );
        assert_eq!(keep_text(0), None, "never purging has nothing to say");

        let report = |removed, failed, why: Option<&str>| Purged {
            removed,
            failed,
            first_error: why.map(str::to_string),
        };
        assert_eq!(purged_text(&report(0, 0, None), 30), None);
        assert_eq!(
            purged_text(&report(12, 0, None), 30),
            Some((
                "Emptied 12 items older than 30 days from the trash".to_string(),
                false
            ))
        );
        assert_eq!(
            purged_text(&report(1, 0, None), 7).map(|(text, _)| text),
            Some("Emptied 1 item older than 7 days from the trash".to_string())
        );
        assert_eq!(
            purged_text(&report(10, 2, Some("x: Permission denied")), 30),
            Some((
                "Emptied 10 items older than 30 days from the trash — \
                 2 old items could not be removed: x: Permission denied"
                    .to_string(),
                true
            ))
        );
        assert_eq!(
            purged_text(&report(0, 1, Some("x: Permission denied")), 30),
            Some((
                "1 old item could not be removed: x: Permission denied".to_string(),
                true
            ))
        );
    }
}
