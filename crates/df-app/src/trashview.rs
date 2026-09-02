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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use df_core::fs::{mime, Entry, Kind};
use df_core::ops::TrashedItem;

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
    let meta = std::fs::symlink_metadata(item.files_path()).ok();
    row_from(item, meta.as_ref())
}

/// The pure half, so the mapping is a table test rather than something you have
/// to delete a file to check.
pub fn row_from(item: &TrashedItem, meta: Option<&std::fs::Metadata>) -> Entry {
    use std::os::unix::fs::MetadataExt;
    let name = item.name.to_string_lossy().into_owned();
    let is_dir = meta.is_some_and(|m| m.is_dir());
    let original_name = item
        .original
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.clone());
    let kind = if is_dir { Kind::Dir } else { Kind::File };
    let mode = meta
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
            meta.map(|m| m.len()).unwrap_or(0)
        },
        // **The deletion date, not the file's own mtime.** It is the date the
        // trash is read for, and putting it here means `, m` sorts by it and
        // the mtime linemode shows it without a second column existing.
        mtime: deleted_at(&item.deleted_at),
        btime: None,
        mode,
        uid: meta.map(|m| m.uid()).unwrap_or(0),
        gid: meta.map(|m| m.gid()).unwrap_or(0),
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
/// Hand-rolled for the same reason the writer is: parsing a fixed-width ini
/// field is arithmetic, and the alternative is a date crate for one line.
/// Anything that does not parse comes back `None`, which the mtime linemode and
/// the mtime sort already tolerate — an item with an unreadable date is still
/// listed and still restorable, which is the only thing that matters.
pub fn deleted_at(text: &str) -> Option<SystemTime> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let num = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month as u32, day as u32);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    if secs >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(secs as u64))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))
    }
}

/// Howard Hinnant's `days_from_civil` — the inverse of the `civil_from_days`
/// [`df_core::ops::trash`] already carries, and the same reason for it: no
/// lookup tables, no leap-year special cases.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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

/// The dialog body for `D` in the trash. Its own sentence because the ordinary
/// one — "Permanently delete — cannot be undone" — is true but incomplete here:
/// what is being deleted is already deleted, and the thing being destroyed is
/// the *chance to get it back*.
pub const PURGE_SUBTITLE: &str = "Destroys them for good — there is no way back from the trash.";

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

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
        let row = row_from(&i, None);
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

    /// A dotfile stays hidden in the trash, judged by the name it had — the
    /// in-trash name of `.bashrc` deleted twice is still `.bashrc_1`, but the
    /// rule must not depend on that.
    #[test]
    fn hidden_files_are_still_hidden_in_the_trash() {
        assert!(
            row_from(
                &item("x", "/home/brian/.bashrc", "2026-08-30T09:15:00"),
                None
            )
            .is_hidden
        );
        assert!(
            !row_from(
                &item("x", "/home/brian/notes.txt", "2026-08-30T09:15:00"),
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
}
