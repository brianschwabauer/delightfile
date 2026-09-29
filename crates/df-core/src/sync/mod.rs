//! Sync — `alt+p`: a copy that proves it arrived, and a mirror.
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
//!    file and each name is `fsync`ed before the copy is done with it; in
//!    [`Mode::Mirror`], then remove the extras — to the trash when one lives on
//!    the destination's filesystem, for good when none does. [`Mode::Update`]
//!    removes nothing, ever.
//! 3. **Verify**. Re-read what was copied (or, [`Verify::Everything`], every
//!    file) on *both* sides, with the page cache dropped first, and compare
//!    SHA-256 digests. Every mismatch is collected rather than the first one
//!    stopping the run, so the result names every bad file.
//!
//! A remote endpoint (`sftp://…`) is not walked here at all: [`rsync`] builds
//! the same plan from `rsync`'s own dry run and carries it out with `rsync`,
//! because a tree walk over SFTP is a round trip per file.
//!
//! [`CopyOptions::durable`]: crate::ops::CopyOptions::durable

mod execute;
mod plan;
pub mod rsync;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

pub use execute::{execute, SyncReport};
pub use plan::{plan, roots};

/// What a sync does about paths the destination has and the source does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Copy what is new or has changed, and never remove anything. The safe
    /// default: a sync that was only meant to top up a backup must not be one
    /// keystroke away from emptying it.
    #[default]
    Update,
    /// Update, then remove every [`Class::Extra`] so the destination ends up
    /// holding exactly what the source holds.
    Mirror,
}

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
    /// Only at the destination. Removed by [`Mode::Mirror`], left alone by
    /// [`Mode::Update`].
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
    /// A file's length: what copying it moves, or what removing it frees.
    /// Zero for everything that is not a file.
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

/// Where a mirror's extras go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Removal {
    /// The trash on the destination's own filesystem, the one `d` uses —
    /// undoable from the trash view.
    #[default]
    Trash,
    /// For good: nowhere on that filesystem can hold a trash (a read-only
    /// mount's root, a server). The card says "to delete" instead of "to
    /// trash" so the difference is read before `Enter`, not after.
    Delete,
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
    /// Where [`Mode::Mirror`] would put the extras.
    pub removal: Removal,
    /// Folders at the destination where the source has a file or a link: a
    /// mirror removes each to make room, and counts it as removed, so the
    /// progress total needs them before the run starts.
    pub folders_in_the_way: u64,
    /// Paths that could not be read, and why — so were never compared or
    /// copied. A **problem**, like a failed copy: the whole promise of a sync
    /// that ends without its card is that the source can now be wiped, and a
    /// file it could not read is a file that would go with it.
    pub skipped: Vec<(PathBuf, String)>,
    /// Sockets, fifos and device nodes in the source: nothing a file manager
    /// can reproduce, so left out by name — and *not* a problem, since there
    /// is nothing on a camera card or in a backup that one of them holds.
    pub specials: Vec<PathBuf>,
    /// When one end is a server, the `rsync` run that carries the plan out.
    /// `None` for a sync on this machine, which this module does itself.
    pub remote: Option<rsync::Transfer>,
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
            removal: Removal::Trash,
            folders_in_the_way: 0,
            skipped: Vec::new(),
            specials: Vec::new(),
            remote: None,
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
    /// below it, with a trailing separator on a folder — the platform's
    /// separator throughout, `photos/2024/` on Unix and `photos\2024\` on
    /// Windows, where `rel` may hold either (joined here with `\`, or read
    /// from rsync's `/`) and printing it as it is would mix the two. The
    /// names are `rel`'s components, so on Unix the label is the bytes it
    /// always was.
    pub fn label(&self, item: &Item) -> String {
        let name = self.roots[item.root]
            .dst
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut label = name;
        for part in item.rel.components() {
            label.push(std::path::MAIN_SEPARATOR);
            label.push_str(&part.as_os_str().to_string_lossy());
        }
        if item.kind == Kind::Dir {
            label.push(std::path::MAIN_SEPARATOR);
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

    /// Whether the destination has anything the source does not.
    pub fn has_extras(&self) -> bool {
        self.items.iter().any(|item| item.class == Class::Extra)
    }

    /// Whether `item` is debris: a `.df-tmp-…` name at the destination, left
    /// by a copy that was killed before its rename.
    ///
    /// It is our own leftover, never the user's file — every temporary name a
    /// copy makes starts that way, and nothing else does — so even an update,
    /// which removes nothing of the user's, clears it, and for good: a partial
    /// copy is not worth a place in the trash. Only for a sync on this
    /// machine; `rsync` keeps its own temporary names and its own house.
    pub fn is_debris(&self, item: &Item) -> bool {
        self.remote.is_none()
            && item.class == Class::Extra
            && item.rel.file_name().is_some_and(|name| {
                crate::platform::os::as_bytes(name)
                    .is_ok_and(|name| name.starts_with(crate::ops::copy::TEMP_PREFIX.as_bytes()))
            })
    }

    /// Whether the destination holds any of a killed copy's leftovers.
    pub fn has_debris(&self) -> bool {
        self.items.iter().any(|item| self.is_debris(item))
    }

    /// Nothing for `mode` to do: nothing to copy, no debris to clear and, for
    /// a mirror, nothing to remove. The card says "Already in sync" and offers
    /// to verify instead.
    pub fn in_sync(&self, mode: Mode) -> bool {
        !self.has_copies() && !self.has_debris() && (mode == Mode::Update || !self.has_extras())
    }

    /// What the card lists for `mode`: the new and the changed, then what goes
    /// — every extra for a mirror, the debris for an update. Unchanged paths
    /// are the ones nobody needs to read.
    pub fn listed(&self, mode: Mode) -> impl Iterator<Item = &Item> {
        let copies = self
            .items
            .iter()
            .filter(|item| matches!(item.class, Class::New | Class::Changed));
        let extras = self.items.iter().filter(move |item| {
            item.class == Class::Extra && (mode == Mode::Mirror || self.is_debris(item))
        });
        copies.chain(extras)
    }

    /// What a run in `mode` removes: for a mirror the topmost extras, deepest
    /// first, each with how many of the counted extras it takes with it; for
    /// an update only the debris ([`SyncPlan::is_debris`]).
    ///
    /// Only the topmost of each run of extras is removed, and its subtree goes
    /// with it — one trashed folder the trash view can put back whole, rather
    /// than its files one by one and then an empty folder. Deepest first, so
    /// that however the list was built a child never outlives the removal of
    /// the directory holding it. One pass over the items, and a lookup per
    /// ancestor: a card full of extras must not be quadratic in them.
    pub fn removals(&self, mode: Mode) -> Vec<(usize, u64)> {
        use std::collections::HashMap;
        if mode == Mode::Update {
            return self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| self.is_debris(item))
                .map(|(index, item)| (index, u64::from(item.leaf)))
                .collect();
        }
        let extras: HashMap<(usize, &Path), usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.class == Class::Extra)
            .map(|(index, item)| ((item.root, item.rel.as_path()), index))
            .collect();
        // Each extra's topmost extra ancestor, which may be itself.
        let top_of = |item: &Item| -> Option<usize> {
            let mut top = extras.get(&(item.root, item.rel.as_path())).copied();
            let mut cursor = item.rel.parent();
            while let Some(rel) = cursor.filter(|rel| !rel.as_os_str().is_empty()) {
                if let Some(&index) = extras.get(&(item.root, rel)) {
                    top = Some(index);
                }
                cursor = rel.parent();
            }
            top
        };
        let mut leaves: HashMap<usize, u64> = HashMap::new();
        for item in self.items.iter().filter(|item| item.class == Class::Extra) {
            if let Some(top) = top_of(item) {
                *leaves.entry(top).or_default() += u64::from(item.leaf);
            }
        }
        let mut tops: Vec<(usize, u64)> = leaves.into_iter().collect();
        tops.sort_by_key(|&(index, _)| {
            (
                std::cmp::Reverse(self.items[index].rel.components().count()),
                index,
            )
        });
        tops
    }
}

/// Whether a trash can live on the filesystem `dest` is on, asked without
/// creating one: the platform's rule ([`crate::platform::trash::available_for`]),
/// which on Linux is the home trash or the mount's own and never a copy across
/// to the home one.
pub fn trash_available(dest: &Path) -> bool {
    crate::platform::trash::available_for(dest)
}

/// The trash a mirror's extras under `dest` go into — made if it has to be
/// ([`crate::platform::trash::for_sync`]).
pub(crate) fn trash_for(dest: &Path) -> crate::Result<crate::ops::Trash> {
    #[cfg(test)]
    if let Some(root) = TEST_TRASH.with(|slot| slot.borrow().clone()) {
        return Ok(crate::ops::Trash::at(root));
    }
    crate::platform::trash::for_sync(dest)
}

#[cfg(test)]
thread_local! {
    /// Where this thread's mirrors trash things, in place of the real choice:
    /// a test must never fill the home trash, nor make a `.Trash-$uid` at the
    /// root of whatever `$TMPDIR` is mounted on.
    static TEST_TRASH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Send this thread's mirrors to the trash at `root`.
#[cfg(test)]
pub(crate) fn trash_at(root: PathBuf) {
    TEST_TRASH.with(|slot| *slot.borrow_mut() = Some(root));
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
