//! File tags: the freedesktop `user.xdg.tags` extended attribute.
//!
//! A tag is a word a person puts on a file — `red`, `work`, `invoice 2026` —
//! and the file carries it, not a database beside it. That is the whole reason
//! the tags are an extended attribute rather than a table in the state file:
//! a tagged file copied to another folder, moved, renamed, or opened by
//! another program that reads the same attribute (Dolphin, Nautilus with an
//! extension, `getfattr`) is still tagged, and a state file that fell out of
//! step with the disk would be a list of tags about files that are gone.
//!
//! # The format
//!
//! The value of `user.xdg.tags` is a comma-separated UTF-8 list, as the
//! freedesktop shared-metadata spec writes it: `red,work,invoice 2026`. Each
//! tag is stored exactly as it was typed — case, spaces inside it and all —
//! so a tag written here reads the same in every other program. A comma cannot
//! be part of a tag, because it is the separator; [`write`] refuses one rather
//! than writing a value that would read back as two tags.
//!
//! Reading is forgiving where writing is strict: whitespace around a comma is
//! trimmed and empty entries are dropped, so `red, work,` written by a hand
//! with `setfattr` reads as the two tags it meant.
//!
//! Tags are compared **case-insensitively** everywhere — `Red` and `red` are
//! one tag, the way Finder has always treated them — while the spelling the
//! file carries is the one shown.
//!
//! # Never through a link
//!
//! Every call here is the `l` form (`lgetxattr`, `lsetxattr`, `llistxattr`,
//! `lremovexattr`), so a symlink is never followed: tagging a link must not
//! quietly tag whatever it points at, which may be in another folder or
//! another person's. Linux does not let a `user.*` attribute sit on a link at
//! all, so a link simply has no tags, and asking to give it some is refused
//! ([`TagError::Link`]).
//!
//! # The syscalls
//!
//! The four calls live in [`crate::platform::xattr`], with the `unsafe` they
//! need. Linux has them; macOS and Windows have no body yet, so there a row
//! has no tags, a tag search finds nothing, a copy carries none, and asking
//! to tag something says tags are not available on this platform.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::platform::xattr::{self, get_raw, list_raw, remove_raw, set_raw};
use crate::DfError;

#[cfg(test)]
pub(crate) use crate::platform::xattr::{refusing, supported_here};

/// The attribute the tags live in (freedesktop's shared-metadata name).
pub const XATTR: &str = "user.xdg.tags";

/// The seven colour tags, in the order a list of them is offered: Finder's
/// set, named as a person would type them. Each is painted with the palette's
/// colour of that name — `orange` is `peach`, `purple` is `mauve` and `grey` is
/// `overlay1` in catppuccin's words — and matched case-insensitively.
pub const COLOURS: [&str; 7] = ["red", "orange", "yellow", "green", "blue", "purple", "grey"];

/// Why tags could not be written.
#[derive(Debug, thiserror::Error)]
pub enum TagError {
    /// A comma inside a tag, which the stored list would read back as two.
    #[error("a tag cannot contain a comma: {0}")]
    Comma(String),
    /// The filesystem does not store `user.*` attributes at all: a FAT card,
    /// most network mounts.
    #[error("This drive can't hold tags")]
    Unsupported,
    /// A symlink, which Linux lets carry no `user.*` attribute.
    #[error("Links can't hold tags")]
    Link,
    #[error("{0}")]
    Io(DfError),
}

/// Whether tags are read on a filesystem of this `f_type` (`statfs`'s magic
/// number).
///
/// Not on a network or FUSE one ([`crate::du::REMOTE_FS_MAGIC`]: NFS, SMB and
/// CIFS, FUSE, 9p, AFS and the rest). A tag read there is a round trip per
/// file — a share of ten thousand files that lists in one `READDIRPLUS` would
/// cost ten thousand more on every scan and every rescan the watcher asks for
/// — so rows there carry no tags, and `T` says why. Every local filesystem
/// reads them; one that keeps no attributes answers with nothing, which costs
/// the one syscall and no more.
pub fn read_on(magic: i64) -> bool {
    !crate::du::REMOTE_FS_MAGIC.contains(&magic)
}

/// Whether tags are read in the directory `dir`: one `statfs`, asked once per
/// scanned directory. A directory that cannot be asked is read as local —
/// the tag reads that follow fail as quietly as the question did.
pub fn read_here(dir: &Path) -> bool {
    crate::du::magic_of(dir).is_none_or(read_on)
}

/// The tags on `path`, in the order they are stored. Empty when it has none,
/// when the filesystem keeps no attributes, or when it cannot be read — the
/// list pane asks for every row, and one unreadable attribute is a row with no
/// dots, not a failed scan.
pub fn read(path: &Path) -> Vec<String> {
    try_read(path).unwrap_or_default()
}

/// [`read`], telling "no tags" apart from "could not ask": the journal's undo
/// has to know that a file is gone rather than that its tags are.
pub fn try_read(path: &Path) -> crate::Result<Vec<String>> {
    match get_raw(path, XATTR) {
        Ok(Some(bytes)) => Ok(parse(&String::from_utf8_lossy(&bytes))),
        Ok(None) => Ok(Vec::new()),
        Err(e) if xattr::quiet(&e) => Ok(Vec::new()),
        Err(e) => Err(DfError::io(path, e)),
    }
}

/// Put exactly `tags` on `path`, replacing whatever it had. An empty list
/// removes the attribute rather than leaving an empty one behind.
///
/// Refused before anything is written when a tag holds a comma. Empty tags
/// are dropped, as [`parse`] drops them.
pub fn write(path: &Path, tags: &[String]) -> Result<(), TagError> {
    if let Some(bad) = tags.iter().find(|tag| tag.contains(',')) {
        return Err(TagError::Comma(bad.clone()));
    }
    if !xattr::AVAILABLE {
        return Err(TagError::Io(DfError::Unsupported("Tags")));
    }
    let tags: Vec<&str> = tags
        .iter()
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty())
        .collect();
    let result = if tags.is_empty() {
        match remove_raw(path, XATTR) {
            Err(e) if xattr::is_absent(&e) => Ok(()),
            other => other,
        }
    } else {
        set_raw(path, XATTR, tags.join(",").as_bytes())
    };
    result.map_err(|e| refusal(path, e))
}

/// What a failed write says: the two refusals a person can do something about
/// named as such, anything else as the error it was.
fn refusal(path: &Path, e: io::Error) -> TagError {
    if xattr::is_unsupported(&e) {
        TagError::Unsupported
    } else if xattr::is_not_permitted(&e)
        && std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_symlink())
    {
        TagError::Link
    } else {
        TagError::Io(DfError::io(path, e))
    }
}

/// The tags a line of text names: split at the commas, each trimmed, empty
/// ones dropped, and a second spelling of a tag already named dropped too
/// (`red, Red` is one tag). Order is kept as written.
///
/// What the `Tags:` prompt's `Enter` makes of its field, and what a stored
/// value is read back as.
pub fn parse(text: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for part in text.split(',') {
        let tag = part.trim();
        if tag.is_empty() || contains(&tags, tag) {
            continue;
        }
        tags.push(tag.to_string());
    }
    tags
}

/// Whether `tags` holds `tag`, in any case.
pub fn contains(tags: &[String], tag: &str) -> bool {
    tags.iter().any(|held| same(held, tag))
}

/// Two spellings of one tag.
pub fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Whether `tags` answers a `#query`: some tag starts with `query`, in any
/// case. An empty query is "any tag at all", so `#` alone narrows a listing
/// to the files that are tagged.
pub fn matches(tags: &[String], query: &str) -> bool {
    if query.is_empty() {
        return !tags.is_empty();
    }
    let query = query.to_lowercase();
    tags.iter()
        .any(|tag| tag.to_lowercase().starts_with(&query))
}

/// The tags every one of `sets` carries — what the prompt opens on for a
/// selection. In the first set's order and spelling.
pub fn shared(sets: &[Vec<String>]) -> Vec<String> {
    let Some((first, rest)) = sets.split_first() else {
        return Vec::new();
    };
    first
        .iter()
        .filter(|tag| rest.iter().all(|set| contains(set, tag)))
        .cloned()
        .collect()
}

/// One item of a selection, after the prompt's edit of the tags it shared
/// with the rest: what was taken out of `shared` is taken out of `current`,
/// what `typed` adds is added to the end, and every other tag the item had
/// stays where it was.
pub fn apply_difference(current: &[String], shared: &[String], typed: &[String]) -> Vec<String> {
    let removed: Vec<&String> = shared.iter().filter(|tag| !contains(typed, tag)).collect();
    let mut out: Vec<String> = current
        .iter()
        .filter(|tag| !removed.iter().any(|gone| same(gone, tag)))
        .cloned()
        .collect();
    for tag in typed {
        if !contains(shared, tag) && !contains(&out, tag) {
            out.push(tag.clone());
        }
    }
    out
}

// ── Carrying tags along with a copy ─────────────────────────────────────────

/// Copy every `user.*` attribute of `src` onto `dst`: what a copy does after
/// the bytes and before its rename, so tags travel with a paste, a move
/// across drives, a trash on another disk and a sync.
///
/// Only `user.*`: `security.*` and `trusted.*` are the kernel's and root's,
/// and `system.*` is ACLs, which ops deliberately does not carry.
///
/// **Never fails the copy.** A destination that refuses attributes — a FAT
/// card says `ENOTSUP` — still gets the file, which is what was asked for; and
/// a source that cannot list its own (a filesystem with no attributes) has
/// none to carry. Returns whether `src` had tags that did not land, which is
/// what the paste toast's "tags not kept on this drive" is made from.
pub(crate) fn carry(src: &Path, dst: &Path) -> bool {
    let names = match list_raw(src) {
        Ok(names) => names,
        Err(e) => {
            if !xattr::quiet(&e) {
                log::debug!("{}: cannot list attributes: {e}", src.display());
            }
            return false;
        }
    };
    let mut dropped = false;
    for name in names.iter().filter(|name| name.starts_with("user.")) {
        let value = match get_raw(src, name) {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(e) => {
                log::debug!("{}: cannot read {name}: {e}", src.display());
                continue;
            }
        };
        if let Err(e) = set_raw(dst, name, &value) {
            if name == XATTR {
                dropped = true;
            }
            if xattr::quiet(&e) {
                log::debug!("{} keeps no {name}: {e}", dst.display());
            } else {
                log::warn!("{}: could not carry {name}: {e}", dst.display());
            }
        }
    }
    dropped
}

// ── Finding tagged files ────────────────────────────────────────────────────

/// Every file and folder under `root` with a tag answering `query` (see
/// [`matches`]), found by walking the tree: the `s` panel's `#tag`.
///
/// There is no index to ask — the tags are on the files — so this is a walk,
/// and it is made cheap where it can be: an entry's attribute *names* are
/// listed first, one `llistxattr`, and the value is read only for the few
/// entries that have `user.xdg.tags` at all. Links are never followed into, so
/// a link to `/` or to its own parent is one entry and not a loop. Hidden
/// entries, and everything under a hidden folder, are left out unless `hidden`
/// says otherwise — the manager's own `.` toggle, as the name search has it.
///
/// A folder on a network or FUSE filesystem ([`read_here`]) is not walked
/// into at all: its tags are not read anywhere else either, and walking a
/// share to read none would be the round trips the gate exists to save.
///
/// Lazy, and stopped by `cancel` between any two entries: the caller pulls
/// matches off it on a worker thread as they are found, as the name search
/// reads `fd`'s lines, and a query that changes stops the walk rather than
/// letting it finish for nobody.
pub fn find(root: &Path, query: &str, hidden: bool, cancel: Arc<AtomicBool>) -> Find {
    Find {
        pending: vec![root.to_path_buf()],
        reading: None,
        query: query.to_string(),
        hidden,
        cancel,
    }
}

/// The walk [`find`] returns: an iterator of matching paths.
pub struct Find {
    /// Folders found and not yet read.
    pending: Vec<PathBuf>,
    /// The folder being read.
    reading: Option<std::fs::ReadDir>,
    query: String,
    hidden: bool,
    cancel: Arc<AtomicBool>,
}

impl Iterator for Find {
    type Item = PathBuf;

    fn next(&mut self) -> Option<PathBuf> {
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return None;
            }
            let Some(reader) = &mut self.reading else {
                let dir = self.pending.pop()?;
                if !read_here(&dir) {
                    continue;
                }
                // A folder that cannot be read is one the walk cannot see
                // into, not a reason to stop looking everywhere else.
                self.reading = std::fs::read_dir(&dir).ok();
                continue;
            };
            let Some(item) = reader.next() else {
                self.reading = None;
                continue;
            };
            let Ok(item) = item else { continue };
            if !self.hidden
                && crate::platform::os::as_bytes(&item.file_name())
                    .is_ok_and(|name| name.first() == Some(&b'.'))
            {
                continue;
            }
            let path = item.path();
            // `file_type` is the directory entry's own type byte: no `stat`,
            // and a link says it is a link rather than what it points at.
            if item.file_type().is_ok_and(|kind| kind.is_dir()) {
                self.pending.push(path.clone());
            }
            if tagged(&path) && matches(&read(&path), &self.query) {
                return Some(path);
            }
        }
    }
}

/// Whether `path` has a `user.xdg.tags` attribute at all, by its list of
/// attribute names alone.
fn tagged(path: &Path) -> bool {
    list_raw(path).is_ok_and(|names| names.iter().any(|name| name == XATTR))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::test_support::TempTree;

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// A tree whose files can hold tags, or `None` (and a line saying why)
    /// on a machine whose `$TMPDIR` cannot.
    fn tree(label: &str) -> Option<TempTree> {
        let tree = TempTree::new(label);
        let probe = tree.file(".probe", b"");
        let ok = supported_here(&probe);
        let _ = std::fs::remove_file(probe);
        ok.then_some(tree)
    }

    /// Tags go on as typed and come back the same; the attribute is the plain
    /// comma list another program reads; and an empty list takes the
    /// attribute away rather than leaving an empty one.
    #[test]
    fn tags_round_trip_through_the_attribute() {
        let Some(t) = tree("tags-round-trip") else {
            return;
        };
        let file = t.file("ünïcödé — 日本語.txt", b"x");
        assert!(read(&file).is_empty(), "a new file has none");

        write(&file, &list(&["red", "invoice 2026", "Work"])).unwrap();
        assert_eq!(read(&file), ["red", "invoice 2026", "Work"]);
        assert_eq!(
            get_raw(&file, XATTR).unwrap().as_deref(),
            Some(b"red,invoice 2026,Work".as_slice()),
            "the freedesktop spelling: commas, no spaces"
        );

        // Another program's hand, with spaces and an empty entry: read as
        // what it meant.
        set_raw(&file, XATTR, b" red , ,blue,").unwrap();
        assert_eq!(read(&file), ["red", "blue"]);

        write(&file, &[]).unwrap();
        assert_eq!(get_raw(&file, XATTR).unwrap(), None, "removed, not emptied");
        assert!(read(&file).is_empty());
        write(&file, &[]).unwrap();
        assert!(
            read(&file).is_empty(),
            "removing none twice is not an error"
        );

        // A folder carries tags as a file does.
        let dir = t.dir("folder");
        write(&dir, &list(&["green"])).unwrap();
        assert_eq!(read(&dir), ["green"]);
    }

    /// Tags are read on every local filesystem, the ones that keep no
    /// attributes included, and on no network or FUSE one.
    #[test]
    fn tags_are_read_on_local_filesystems_only() {
        for (magic, name) in [
            (0xEF53, "ext4"),
            (0x9123_683E, "btrfs"),
            (0x5846_5342, "xfs"),
            (0x0102_1994, "tmpfs"),
            (0xF2F5_2010, "f2fs"),
            (0x2011_BAB0, "exfat"),
            (0x4D44, "vfat"),
        ] {
            assert!(read_on(magic), "{name} is local");
        }
        for (magic, name) in [
            (0x6969, "NFS"),
            (0xFF53_4D42, "CIFS"),
            (0xFE53_4D42, "SMB2"),
            (0x517B, "SMBFS"),
            (0x6573_5546, "FUSE"),
            (0x0102_1997, "9p"),
            (0x5346_414F, "AFS"),
            (0x6B6C, "kAFS"),
        ] {
            assert!(!read_on(magic), "{name} is not read");
        }
        assert!(read_here(Path::new(env!("CARGO_MANIFEST_DIR"))));
        assert!(
            read_here(Path::new("/nonexistent-df-tags-probe")),
            "a directory that cannot be asked is read as local"
        );
    }

    /// A comma would be read back as two tags, so it is refused before
    /// anything is written — and the file keeps what it had.
    #[test]
    fn a_comma_inside_a_tag_is_refused() {
        let Some(t) = tree("tags-comma") else {
            return;
        };
        let file = t.file("a.txt", b"x");
        write(&file, &list(&["red"])).unwrap();
        let err = write(&file, &list(&["blue", "a,b"])).unwrap_err();
        assert!(
            matches!(&err, TagError::Comma(tag) if tag == "a,b"),
            "{err}"
        );
        assert_eq!(read(&file), ["red"], "nothing was written");
    }

    /// A link has no tags of its own and cannot be given any — and asking
    /// never reaches through it to the file it points at.
    #[test]
    fn a_link_is_never_followed() {
        let Some(t) = tree("tags-link") else {
            return;
        };
        let file = t.file("target.txt", b"x");
        write(&file, &list(&["red"])).unwrap();
        let link = t.symlink(&file, "link");
        assert!(
            read(&link).is_empty(),
            "the target's tags are not the link's"
        );
        let err = write(&link, &list(&["blue"])).unwrap_err();
        assert!(matches!(err, TagError::Link), "{err}");
        assert_eq!(read(&file), ["red"], "the target was not touched");
    }

    /// What the prompt makes of a line: trimmed, empties gone, the second
    /// spelling of a tag gone, order as typed.
    #[test]
    fn a_typed_line_is_a_list() {
        assert_eq!(
            parse("  red,work , ,Red,  invoice 2026 ,"),
            ["red", "work", "invoice 2026"]
        );
        assert!(parse("").is_empty());
        assert!(parse(" , ,").is_empty());
    }

    /// `#red` is a prefix, in any case; `#` alone is "tagged at all".
    #[test]
    fn a_query_matches_the_start_of_any_tag() {
        let tags = list(&["Invoice 2026", "red"]);
        assert!(matches(&tags, "inv"));
        assert!(matches(&tags, "INVOICE 2"));
        assert!(matches(&tags, "red"));
        assert!(!matches(&tags, "voice"), "a prefix, not a substring");
        assert!(!matches(&tags, "blue"));
        assert!(matches(&tags, ""), "any tag at all");
        assert!(!matches(&[], ""), "an untagged file is not tagged");
    }

    /// A selection opens on what every item shares, and the edit to that is
    /// applied to each item without touching its other tags.
    #[test]
    fn a_selection_is_edited_by_its_difference() {
        let a = list(&["red", "work", "draft"]);
        let b = list(&["Work", "blue", "red"]);
        let shared = shared(&[a.clone(), b.clone()]);
        assert_eq!(
            shared,
            ["red", "work"],
            "the first item's order and spelling"
        );

        // `work` taken out, `urgent` put in.
        let typed = parse("red, urgent");
        assert_eq!(
            apply_difference(&a, &shared, &typed),
            ["red", "draft", "urgent"]
        );
        assert_eq!(
            apply_difference(&b, &shared, &typed),
            ["blue", "red", "urgent"]
        );
        // A tag typed that one item already had is not added twice.
        let typed = parse("red, work, blue");
        assert_eq!(apply_difference(&b, &shared, &typed), b);
        assert!(super::shared(&[]).is_empty());
    }

    /// The walk finds tagged files and folders at any depth, leaves hidden
    /// ones alone unless asked, and never follows a link.
    #[test]
    fn the_walk_finds_nested_matches_and_skips_hidden_ones() {
        let Some(t) = tree("tags-find") else {
            return;
        };
        let top = t.file("top.txt", b"x");
        let deep = t.file("a/b/c/deep.txt", b"x");
        let folder = t.dir("a/tagged-folder");
        let other = t.file("a/other.txt", b"x");
        let hidden = t.file(".secret/inside.txt", b"x");
        let dotfile = t.file("a/.dotfile", b"x");
        t.file("a/untagged.txt", b"x");
        for path in [&top, &deep, &folder, &hidden, &dotfile] {
            write(path, &list(&["Red"])).unwrap();
        }
        write(&other, &list(&["blue"])).unwrap();
        // A link back up the tree: followed, it would be a loop.
        t.symlink(t.path(), "a/b/loop");

        let found = |query: &str, hidden: bool| {
            let mut paths: Vec<PathBuf> =
                find(t.path(), query, hidden, Arc::new(AtomicBool::new(false))).collect();
            paths.sort();
            paths
        };
        let mut shown = vec![deep.clone(), folder.clone(), top.clone()];
        shown.sort();
        assert_eq!(found("re", false), shown);
        let mut all = vec![deep, folder, top, hidden, dotfile];
        all.sort();
        assert_eq!(found("RED", true), all);
        assert_eq!(found("blue", false), vec![other]);
        assert!(found("green", true).is_empty());
    }

    /// A cancelled walk stops at the next entry, found or not.
    #[test]
    fn the_walk_stops_when_cancelled() {
        let Some(t) = tree("tags-find-cancel") else {
            return;
        };
        for n in 0..20 {
            let file = t.file(format!("d{n}/f.txt"), b"x");
            write(&file, &list(&["red"])).unwrap();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let mut walk = find(t.path(), "red", false, Arc::clone(&cancel));
        assert!(walk.next().is_some());
        assert!(walk.next().is_some());
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(walk.next(), None, "nothing after the cancel");
        assert_eq!(walk.next(), None);
    }

    /// Every `user.*` attribute is carried, and a destination that refuses
    /// them says the tags did not land rather than failing.
    #[test]
    fn carrying_copies_every_user_attribute() {
        let Some(t) = tree("tags-carry") else {
            return;
        };
        let src = t.file("src.txt", b"x");
        write(&src, &list(&["red", "work"])).unwrap();
        set_raw(&src, "user.comment", b"from the camera").unwrap();
        let dst = t.file("dst.txt", b"x");
        assert!(!carry(&src, &dst), "nothing was refused");
        assert_eq!(read(&dst), ["red", "work"]);
        assert_eq!(
            get_raw(&dst, "user.comment").unwrap().as_deref(),
            Some(b"from the camera".as_slice())
        );

        let card = t.file("card.txt", b"x");
        assert!(refusing(|| carry(&src, &card)), "the tags did not land");
        assert!(read(&card).is_empty());

        let plain = t.file("plain.txt", b"x");
        assert!(
            !refusing(|| carry(&plain, &card)),
            "a file with no tags lost none"
        );
    }
}
