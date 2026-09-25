//! Sync — `alt+p`: a copy that proves it arrived.
//!
//! PLAN §5 promised copies "with optional verify", and a paste cannot keep
//! that promise. A paste copies everything it is given, writes into the page
//! cache and calls itself done, and checks sizes afterwards only when a
//! cross-device move is about to delete its source. That is the right paste —
//! it is fast, and it is something the user watches land — and it is the wrong
//! tool for the job a card of photos or a backup disk is: copy *only what
//! differs*, make sure it is on the medium rather than in RAM, and read it back
//! to prove it.
//!
//! So a sync is its own verb over the same clipboard, in three steps:
//!
//! 1. **Plan** ([`plan`]). Walk each yanked tree beside its destination and
//!    classify every path: [`Class::New`], [`Class::Changed`],
//!    [`Class::Unchanged`], or — at the destination only —
//!    [`Class::Extra`]. The walk touches nothing, and the UI shows its answer
//!    as the card's summary before anything is written.
//! 2. **Execute** ([`execute`]). Copy the new and the changed through
//!    [`crate::ops::copy`]'s machinery with [`CopyOptions::durable`] on, so each
//!    file and each name is `fsync`ed before the copy is done with it. Nothing
//!    is ever removed: an extra stays where it is.
//! 3. **Verify**. Re-read what was copied (or, [`Verify::Everything`], every
//!    file) on *both* sides, with the page cache dropped first, and compare
//!    SHA-256 digests. Every mismatch is collected rather than the first one
//!    stopping the run, so the result names every bad file.
//!
//! [`CopyOptions::durable`]: crate::ops::CopyOptions::durable

mod execute;
mod plan;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

pub use execute::{execute, SyncReport};
pub use plan::{plan, roots};

/// How much the verify pass reads back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verify {
    /// Every file this run copied, on both sides: proof that *this* copy
    /// arrived, at the cost of reading it twice.
    #[default]
    Copied,
    /// Every file in the source against its destination, copied or not — the
    /// way to find out whether an old copy has rotted.
    Everything,
}

/// How hard the planner looks before it calls a pair of files the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncOptions {
    /// Hash both sides of every pair the quick comparison calls equal. Off,
    /// size and modification time decide, which is what `rsync` does without
    /// `--checksum` and is right almost always; on, a file whose bytes changed
    /// under the same size and date — a bit flipped on a failing card, an
    /// editor that restored the timestamp — is found and copied again.
    pub content: bool,
}

/// What the planner decided about one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Only in the source.
    New,
    /// In both, and different: another size, a date more than
    /// [`MTIME_SLACK`] apart, another link target, another kind of thing — or,
    /// with [`SyncOptions::content`], other bytes.
    Changed,
    /// In both, and the same as far as the comparison looked.
    Unchanged,
    /// Only at the destination. A sync leaves it where it is.
    Extra,
}

/// What a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    /// A socket, fifo or device node: nothing a file manager can reproduce.
    Special,
}

impl Kind {
    pub(crate) fn of(meta: &std::fs::Metadata) -> Kind {
        let kind = meta.file_type();
        if kind.is_symlink() {
            Kind::Symlink
        } else if kind.is_dir() {
            Kind::Dir
        } else if kind.is_file() {
            Kind::File
        } else {
            Kind::Special
        }
    }
}

/// How far apart two modification times may be and still be the same one.
///
/// Two seconds, because that is how coarse the destinations people sync to
/// are. FAT — every SD card and most USB sticks — stores modification times
/// in two-second steps, some network filesystems in whole seconds, and a copy
/// whose time was set to the nanosecond reads back rounded. A tighter window
/// would call every file on a card changed on every run, and a sync that
/// recopies everything is a sync nobody runs twice.
pub const MTIME_SLACK: std::time::Duration = std::time::Duration::from_secs(2);

/// One yanked path and where it lands: `dest/<its name>`, the same name a
/// paste gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub src: PathBuf,
    pub dst: PathBuf,
}

/// One classified path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Which [`Root`] it is under, by index into [`SyncPlan::roots`].
    pub root: usize,
    /// Its path below that root; empty for the root itself.
    pub rel: PathBuf,
    pub class: Class,
    /// What it is in the source — or, for an [`Class::Extra`], at the
    /// destination, since that is the only side it is on.
    pub kind: Kind,
    /// A file's length: what copying it moves. Zero for everything that is
    /// not a file.
    pub bytes: u64,
    /// Whether the counts count it: anything that is not a directory, and a
    /// directory with nothing under it. A folder of twelve new photos is
    /// twelve new things, not thirteen, and an empty new folder is still one.
    pub leaf: bool,
}

/// A count and its bytes, for one [`Class`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub count: u64,
    pub bytes: u64,
}

/// What a sync would do, before it does any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPlan {
    pub roots: Vec<Root>,
    /// The directory the sync lands in, as the user named it.
    pub dest_dir: PathBuf,
    pub options: SyncOptions,
    /// Every path, in walk order: each directory before what is under it,
    /// names sorted within a directory, and a directory's extras after its
    /// own entries. Unchanged paths are kept too, because
    /// [`Verify::Everything`] reads them.
    pub items: Vec<Item>,
    pub new: Tally,
    pub changed: Tally,
    pub unchanged: Tally,
    pub extra: Tally,
    /// Source paths the sync leaves alone, and why: special files, and
    /// anything that could not be read. Named rather than dropped, because a
    /// sync that silently skipped a folder it could not open would report a
    /// copy that is not one.
    pub skipped: Vec<(PathBuf, String)>,
}

impl SyncPlan {
    /// A plan over `roots` with nothing classified yet.
    pub(crate) fn empty(roots: Vec<Root>, dest_dir: PathBuf, options: SyncOptions) -> SyncPlan {
        SyncPlan {
            roots,
            dest_dir,
            options,
            items: Vec::new(),
            new: Tally::default(),
            changed: Tally::default(),
            unchanged: Tally::default(),
            extra: Tally::default(),
            skipped: Vec::new(),
        }
    }

    /// Fill the four tallies from the items' leaves.
    pub(crate) fn count(&mut self) {
        let mut tallies = [Tally::default(); 4];
        for item in self.items.iter().filter(|item| item.leaf) {
            let slot = match item.class {
                Class::New => 0,
                Class::Changed => 1,
                Class::Unchanged => 2,
                Class::Extra => 3,
            };
            tallies[slot].count += 1;
            tallies[slot].bytes += item.bytes;
        }
        [self.new, self.changed, self.unchanged, self.extra] = tallies;
    }

    /// Where `item` is in the source (or, for an extra, would have been).
    pub fn src_of(&self, item: &Item) -> PathBuf {
        under(&self.roots[item.root].src, &item.rel)
    }

    /// Where `item` is, or will be, at the destination.
    pub fn dst_of(&self, item: &Item) -> PathBuf {
        under(&self.roots[item.root].dst, &item.rel)
    }

    /// `item` as a person reads it on the card: the root's name and the path
    /// below it, with a trailing `/` on a folder.
    pub fn label(&self, item: &Item) -> String {
        let name = self.roots[item.root]
            .dst
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut label = if item.rel.as_os_str().is_empty() {
            name
        } else {
            format!("{name}/{}", item.rel.to_string_lossy())
        };
        if item.kind == Kind::Dir {
            label.push('/');
        }
        label
    }

    /// The bytes a run would copy.
    pub fn bytes_to_copy(&self) -> u64 {
        self.new.bytes + self.changed.bytes
    }

    /// Whether anything is new or changed — a folder included, which the
    /// counts leave out when there is something under it.
    pub fn has_copies(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item.class, Class::New | Class::Changed))
    }

    /// Nothing to copy. The card says "Already in sync" and offers to verify
    /// instead.
    pub fn in_sync(&self) -> bool {
        !self.has_copies()
    }

    /// What the card lists: the new and the changed. Unchanged paths are the
    /// ones nobody needs to read.
    pub fn listed(&self) -> impl Iterator<Item = &Item> {
        self.items
            .iter()
            .filter(|item| matches!(item.class, Class::New | Class::Changed))
    }
}

/// `base` with `rel` under it — or `base` itself for an empty `rel`, which
/// `Path::join` would give a trailing slash (and a trailing slash on a symlink
/// makes every call that follows it act on the link's target).
fn under(base: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(rel)
    }
}
