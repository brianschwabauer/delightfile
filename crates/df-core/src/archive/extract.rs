//! What an extraction *would* do — the half that can be tested without a
//! decompressor.
//!
//! Planning and executing are separated the same way [`mod@crate::ops::paste`]
//! separates them, and for the same reason: every decision worth getting right
//! is in the plan, and the plan is a pure function. Which entries are selected,
//! where each one lands, which are refused for an unsafe name, which would
//! overwrite something already on disk, and how many bytes the whole thing needs
//! — all of that is settled and inspectable before a single byte is written.
//!
//! ## The safety contract, enforced here
//!
//! [`plan_extract`] never emits a destination for an entry with
//! [`ArchiveEntry::unsafe_name`](super::ArchiveEntry::unsafe_name) set. They go
//! into [`ExtractPlan::skipped`] with a reason, so the UI can say "3 entries were
//! skipped: unsafe paths" rather than quietly producing fewer files than the
//! listing showed. That is the *only* correct handling short of renaming them,
//! and renaming is a decision for a person, not a default.
//!
//! As a second, independent check, every destination is verified to still be
//! under the destination directory after joining. It is redundant with the name
//! flag by construction — and it is exactly the kind of redundancy worth having
//! in the one function whose bug is a remote write to `~/.ssh`.
//!
//! ## What is deliberately not here (yet)
//!
//! The execution. See [`super`]'s essay: decompressing a zip member means an
//! inflate implementation this crate does not have, and the alternative — shelling
//! out to `unzip` — is a design decision about trusting a second tool's path
//! handling rather than our own. The plan is the part that is ready.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::tree::{ArchiveEntry, ArchiveTree};

/// Why an entry will not be extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Absolute, traversing, or colliding with `.git`. See [`super::tree`].
    UnsafeName,
    /// Needs a password this build has no way to ask for.
    Encrypted,
    /// The join escaped the destination anyway. Unreachable given the flag
    /// above; kept because "unreachable" is a claim, not a guarantee.
    Escapes,
}

impl SkipReason {
    pub fn label(self) -> &'static str {
        match self {
            SkipReason::UnsafeName => "unsafe path",
            SkipReason::Encrypted => "encrypted",
            SkipReason::Escapes => "escapes the destination",
        }
    }
}

/// One file or directory to create.
#[derive(Debug, Clone)]
pub struct ExtractItem {
    /// The path inside the archive — what the extractor asks the archive for.
    pub inner: String,
    /// Where it lands. Always under [`ExtractPlan::dest_dir`].
    pub dest: PathBuf,
    pub is_dir: bool,
    pub len: u64,
    /// Something already exists at `dest`. The UI resolves this the way paste
    /// does (skip, replace, keep both) before anything runs.
    pub conflict: bool,
}

/// A settled extraction.
#[derive(Debug, Clone)]
pub struct ExtractPlan {
    pub archive: PathBuf,
    pub dest_dir: PathBuf,
    /// Directories first, then files, each group in listing order — so creating
    /// them top to bottom never needs a parent that does not exist yet.
    pub items: Vec<ExtractItem>,
    pub skipped: Vec<(String, SkipReason)>,
    /// Uncompressed bytes across every file in the plan. The progress total.
    pub total_len: u64,
    pub conflicts: usize,
}

impl ExtractPlan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn file_count(&self) -> usize {
        self.items.iter().filter(|i| !i.is_dir).count()
    }
}

/// Decide what extracting `selection` into `dest_dir` would do.
///
/// `selection` is inner paths as [`ArchiveTree::entries`] reports them; a
/// selected directory brings everything beneath it. An empty selection means the
/// whole archive, which is what `x` on the archive row does.
///
/// Paths are laid out under `dest_dir` keeping their structure, so a selection of
/// `src/a.rs` lands at `dest_dir/src/a.rs`. Preserving the prefix is the
/// behaviour every archiver has and the only one that keeps a multi-file
/// selection from colliding.
pub fn plan_extract(tree: &ArchiveTree, selection: &[&str], dest_dir: &Path) -> ExtractPlan {
    let wanted = chosen(tree, selection);

    let mut items = Vec::new();
    let mut skipped = Vec::new();
    let mut total_len = 0u64;
    let mut conflicts = 0usize;

    // Directories before files: an extractor walking this list in order never
    // has to create a parent on the fly, which is what makes a partial failure
    // leave a sane tree behind.
    for pass_dirs in [true, false] {
        for entry in tree.all() {
            if entry.is_dir != pass_dirs || !wanted.contains(entry.path.as_str()) {
                continue;
            }
            if entry.unsafe_name {
                skipped.push((entry.path.clone(), SkipReason::UnsafeName));
                continue;
            }
            if entry.encrypted && !entry.is_dir {
                skipped.push((entry.path.clone(), SkipReason::Encrypted));
                continue;
            }
            let Some(dest) = destination_for(entry, dest_dir) else {
                skipped.push((entry.path.clone(), SkipReason::Escapes));
                continue;
            };
            let conflict = crate::ops::exists(&dest);
            if conflict {
                conflicts += 1;
            }
            if !entry.is_dir {
                total_len = total_len.saturating_add(entry.len);
            }
            items.push(ExtractItem {
                inner: entry.path.clone(),
                dest,
                is_dir: entry.is_dir,
                len: entry.len,
                conflict,
            });
        }
    }

    ExtractPlan {
        archive: tree.path().to_path_buf(),
        dest_dir: dest_dir.to_path_buf(),
        items,
        skipped,
        total_len,
        conflicts,
    }
}

/// The set of inner paths a selection covers, subtrees included.
fn chosen<'a>(tree: &'a ArchiveTree, selection: &[&str]) -> HashSet<&'a str> {
    let mut out = HashSet::new();
    if selection.is_empty() {
        for entry in tree.all() {
            out.insert(entry.path.as_str());
        }
        return out;
    }
    // Normalized once, so `src/` and `src` and `./src` all select the same
    // subtree — a caller assembling these from a UI path bar should not have to
    // be careful about the trailing slash.
    let roots: Vec<String> = selection
        .iter()
        .filter_map(|s| super::tree::normalize(s))
        .collect();
    for entry in tree.all() {
        let path = entry.path.as_str();
        let hit = roots.iter().any(|root| {
            path == root
                || (path.len() > root.len()
                    && path.starts_with(root.as_str())
                    && path.as_bytes()[root.len()] == b'/')
        });
        if hit {
            out.insert(path);
        }
    }
    out
}

/// A sanity check a caller can run on one entry without building a plan.
///
/// The same rule [`plan_extract`] applies, exposed so a preview or a drag can
/// grey out an entry that could never be extracted.
pub fn destination_for(entry: &ArchiveEntry, dest_dir: &Path) -> Option<PathBuf> {
    // Three checks where one would do, in increasing order of paranoia. The
    // second exists because the flag is set at listing time and this function
    // may be handed an entry a caller assembled; the third because
    // `Path::starts_with` is lexical, so `/tmp/out/..` passes it — which is
    // exactly why it cannot be the *only* check.
    if entry.unsafe_name || super::tree::name_is_unsafe(&entry.path) {
        return None;
    }
    let dest = dest_dir.join(&entry.path);
    dest.starts_with(dest_dir).then_some(dest)
}
