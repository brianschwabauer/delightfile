//! The planner: walk each source beside its destination and say what differs.
//!
//! Read-only from end to end. It stats, lists directories and — only with
//! [`SyncOptions::content`] — reads files, and writes nothing, which is what
//! lets the card show its answer before anyone has agreed to anything. A walk
//! of a 200,000-file tree takes seconds, so it runs as a job and asks `stop`
//! before every entry and every chunk it hashes, the way
//! [`crate::archive::list_until`] asks before every read.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::ops::{exists, file_name, is_ancestor_resolved, is_real_dir, normalize, same_file};
use crate::{DfError, Result};

use super::{Class, Item, Kind, Removal, Root, SyncOptions, SyncPlan, MTIME_SLACK};

/// Where each source lands, or why the sync cannot happen at all.
///
/// Each yanked path goes to `dest_dir/<its name>` — a folder `photos` syncs to
/// `dest/photos`, a file `a.jpg` to `dest/a.jpg` — which is the name a paste
/// would give it. The refusals are the ones no answer could make safe:
///
/// - a folder into itself or below itself, which would walk its own output;
/// - a path onto itself, which has nothing to sync and, as a mirror, would
///   read its own contents as extras;
/// - a destination that *holds* the source (`dest/photos` when the source is
///   `dest/photos/photos`), where a mirror would remove the source as an
///   extra of the folder it lives in;
/// - two sources with one name, which would both land in the same place and
///   each mirror the other away.
///
/// Resolved rather than lexical, as [`crate::ops::paste`]'s rails are: a
/// destination spelled through a symlink is still inside the source.
pub fn roots(sources: &[PathBuf], dest_dir: &Path) -> Result<Vec<Root>> {
    if crate::ops::is_url(dest_dir) || sources.iter().any(|src| crate::ops::is_url(src)) {
        return Err(DfError::Op(
            "a sync with a server goes through rsync, not this walk".to_string(),
        ));
    }
    let dest_dir = normalize(dest_dir);
    if !dest_dir.is_dir() {
        return Err(DfError::Op(format!(
            "{} is not a folder",
            dest_dir.display()
        )));
    }
    let mut names: HashSet<OsString> = HashSet::new();
    let mut roots = Vec::with_capacity(sources.len());
    for src in sources {
        let src = normalize(src);
        if !exists(&src) {
            return Err(DfError::io(
                &src,
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            ));
        }
        let name = file_name(&src)?;
        if !names.insert(name.to_os_string()) {
            return Err(DfError::Op(format!(
                "two of the yanked items are called {}, and a sync would put both in one place",
                name.to_string_lossy()
            )));
        }
        let dst = dest_dir.join(name);
        if same_file(&src, &dst) {
            return Err(DfError::Op(format!(
                "cannot sync {} onto itself",
                name.to_string_lossy()
            )));
        }
        if is_real_dir(&src) && is_ancestor_resolved(&src, &dest_dir) {
            return Err(DfError::Op("cannot sync a folder into itself".to_string()));
        }
        if is_ancestor_resolved(&dst, &src) {
            return Err(DfError::Op(format!(
                "cannot sync {} into a folder that holds it",
                name.to_string_lossy()
            )));
        }
        roots.push(Root { src, dst });
    }
    Ok(roots)
}

/// Work out what syncing `sources` into `dest_dir` would do.
///
/// `stop` is asked before every entry and every hashed chunk; once it says
/// yes the walk ends with [`DfError::Cancelled`] and no plan. `seen` is told
/// each time an entry has been looked at, so the card can count up while it
/// waits.
pub fn plan(
    sources: &[PathBuf],
    dest_dir: &Path,
    options: SyncOptions,
    stop: &dyn Fn() -> bool,
    seen: &dyn Fn(u64),
) -> Result<SyncPlan> {
    walk(sources, dest_dir, options, stop, seen, &children)
}

/// [`plan`], reading the destination's folders through `list`: the seam a test
/// uses to be a case-folding card on a case-sensitive disk.
pub(super) fn walk(
    sources: &[PathBuf],
    dest_dir: &Path,
    options: SyncOptions,
    stop: &dyn Fn() -> bool,
    seen: &dyn Fn(u64),
    list: &dyn Fn(&Path) -> Result<Vec<OsString>>,
) -> Result<SyncPlan> {
    let roots = roots(sources, dest_dir)?;
    let mut walk = Walk {
        plan: SyncPlan::empty(roots.clone(), normalize(dest_dir), options),
        stop,
        seen,
        list,
    };
    for (index, root) in roots.iter().enumerate() {
        walk.visit(index, PathBuf::new(), &root.src, &root.dst, false)?;
    }
    let mut plan = walk.plan;
    plan.count();
    plan.removal = if super::trash_available(&plan.dest_dir) {
        Removal::Trash
    } else {
        Removal::Delete
    };
    Ok(plan)
}

struct Walk<'a> {
    plan: SyncPlan,
    stop: &'a dyn Fn() -> bool,
    seen: &'a dyn Fn(u64),
    /// How a destination folder is listed.
    list: &'a dyn Fn(&Path) -> Result<Vec<OsString>>,
}

impl Walk<'_> {
    fn check(&self) -> Result<()> {
        if (self.stop)() {
            return Err(DfError::Cancelled);
        }
        Ok(())
    }

    /// Classify `src` against `dst`, and everything under them.
    ///
    /// `absent` says the destination is already known not to exist — its
    /// parent was new — which saves a `stat` per file on the whole of a new
    /// tree, the common case for a first sync onto an empty card.
    fn visit(
        &mut self,
        root: usize,
        rel: PathBuf,
        src: &Path,
        dst: &Path,
        absent: bool,
    ) -> Result<()> {
        self.check()?;
        (self.seen)(1);
        let src_meta = match std::fs::symlink_metadata(src) {
            Ok(meta) => meta,
            Err(e) => {
                self.plan.skipped.push((src.to_path_buf(), e.to_string()));
                return Ok(());
            }
        };
        let kind = Kind::of(&src_meta);
        if kind == Kind::Special {
            self.plan
                .skipped
                .push((src.to_path_buf(), "not a file, folder or link".to_string()));
            return Ok(());
        }
        let dst_meta = if absent {
            None
        } else {
            match std::fs::symlink_metadata(dst) {
                Ok(meta) => Some(meta),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    self.plan.skipped.push((dst.to_path_buf(), e.to_string()));
                    return Ok(());
                }
            }
        };
        let class = match &dst_meta {
            None => Class::New,
            Some(meta) if Kind::of(meta) != kind => {
                if Kind::of(meta) == Kind::Dir {
                    self.plan.folders_in_the_way += 1;
                }
                Class::Changed
            }
            Some(meta) => self.compare(kind, src, &src_meta, dst, meta)?,
        };
        let index = self.plan.items.len();
        self.plan.items.push(Item {
            root,
            rel: rel.clone(),
            class,
            kind,
            bytes: if kind == Kind::File {
                src_meta.len()
            } else {
                0
            },
            leaf: true,
        });
        if kind != Kind::Dir {
            return Ok(());
        }

        let names = match children(src) {
            Ok(names) => names,
            Err(e) => {
                // The folder is listed as it is and nothing is said about
                // what is in it — above all, nothing at the destination is
                // called extra because a folder here could not be read.
                self.plan.skipped.push((src.to_path_buf(), e.to_string()));
                return Ok(());
            }
        };
        self.plan.items[index].leaf = names.is_empty();
        // Below a new folder — or one that replaces a file — nothing exists
        // yet, so nothing below needs asking about.
        let below_absent = class != Class::Unchanged;
        // Where each child's own item went, for a case twin to find it.
        let mut placed: HashMap<OsString, usize> = HashMap::new();
        for name in &names {
            let at = self.plan.items.len();
            self.visit(
                root,
                rel.join(name),
                &src.join(name),
                &dst.join(name),
                below_absent,
            )?;
            if self.plan.items.len() > at {
                placed.insert(name.clone(), at);
            }
        }
        if class == Class::Unchanged {
            self.extras_in(root, &rel, dst, &names, &placed)?;
        }
        Ok(())
    }

    /// Same kind on both sides: are they the same thing?
    fn compare(
        &self,
        kind: Kind,
        src: &Path,
        src_meta: &Metadata,
        dst: &Path,
        dst_meta: &Metadata,
    ) -> Result<Class> {
        let same = match kind {
            Kind::File => {
                quick_same(src_meta, dst_meta)
                    && (!self.plan.options.content || same_contents(src, dst, self.stop)?)
            }
            // A link is its target text: two links to the same place are the
            // same link, whatever their dates say.
            Kind::Symlink => match (std::fs::read_link(src), std::fs::read_link(dst)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            },
            // A folder is there or it is not; what is in it is its children's
            // business.
            Kind::Dir => true,
            Kind::Special => false,
        };
        Ok(if same {
            Class::Unchanged
        } else {
            Class::Changed
        })
    }

    /// The names in `dst` that the source's `names` does not have, each an
    /// extra with everything under it — except a source name listed in the
    /// destination's own case ([`case_twins`]), which is the same file.
    ///
    /// A twin is never an extra, and its source item is marked changed, so the
    /// run rewrites it: the copy goes to a temporary name and is renamed over
    /// the folded one, and the file comes out spelled the way the source
    /// spells it — which is also what makes the next plan find it unchanged.
    fn extras_in(
        &mut self,
        root: usize,
        rel: &Path,
        dst: &Path,
        names: &[OsString],
        placed: &HashMap<OsString, usize>,
    ) -> Result<()> {
        let theirs = match (self.list)(dst) {
            Ok(theirs) => theirs,
            Err(e) => {
                self.plan.skipped.push((dst.to_path_buf(), e.to_string()));
                return Ok(());
            }
        };
        let ours: HashSet<&OsString> = names.iter().collect();
        let twins = case_twins(names, &theirs);
        for name in theirs.iter().filter(|name| !ours.contains(name)) {
            if let Some(twin) = twins.get(name) {
                if let Some(&index) = placed.get(twin) {
                    let item = &mut self.plan.items[index];
                    if item.kind != Kind::Dir && item.class == Class::Unchanged {
                        item.class = Class::Changed;
                    }
                }
                continue;
            }
            self.extra(root, rel.join(name), &dst.join(name))?;
        }
        Ok(())
    }

    fn extra(&mut self, root: usize, rel: PathBuf, dst: &Path) -> Result<()> {
        self.check()?;
        (self.seen)(1);
        let meta = match std::fs::symlink_metadata(dst) {
            Ok(meta) => meta,
            // Gone between the listing and the stat: nothing left to remove.
            Err(_) => return Ok(()),
        };
        let kind = Kind::of(&meta);
        let index = self.plan.items.len();
        self.plan.items.push(Item {
            root,
            rel: rel.clone(),
            class: Class::Extra,
            kind,
            bytes: if kind == Kind::File { meta.len() } else { 0 },
            leaf: true,
        });
        if kind == Kind::Dir {
            let names = children(dst).unwrap_or_default();
            self.plan.items[index].leaf = names.is_empty();
            for name in &names {
                self.extra(root, rel.join(name), &dst.join(name))?;
            }
        }
        Ok(())
    }
}

/// The destination names that are a source name in other case, each mapped to
/// that source name: what a disk that folds case — FAT, exFAT, every camera
/// card — lists for a file the source spells differently.
///
/// Such a disk answers a `stat` of `photo.jpg` with its `Photo.JPG`, so the
/// pair compares as one file; then it lists only `Photo.JPG`, which is nowhere
/// in the source's names. Read as an extra, a mirror would trash the one copy
/// of the file it had just called unchanged. So a destination name is a twin
/// when it is not a source name itself, it folds ([`fold`]) to the same text
/// as one, and that source name's own spelling is *not* in the destination's
/// listing — a case-sensitive disk holding both `photo.jpg` and `Photo.JPG`
/// has two files, and the second is a real extra. Two source names that fold
/// alike twin nothing: there is no telling which one the disk is holding.
pub(super) fn case_twins(ours: &[OsString], theirs: &[OsString]) -> HashMap<OsString, OsString> {
    let listed: HashSet<&OsString> = theirs.iter().collect();
    let mut folded: HashMap<String, Option<&OsString>> = HashMap::new();
    for name in ours {
        let Some(text) = name.to_str() else { continue };
        folded
            .entry(fold(text))
            .and_modify(|seen| *seen = None)
            .or_insert(Some(name));
    }
    let mut twins = HashMap::new();
    for name in theirs.iter().filter(|name| !ours.contains(name)) {
        let Some(text) = name.to_str() else { continue };
        if let Some(Some(source)) = folded.get(&fold(text)) {
            if !listed.contains(source) {
                twins.insert(name.clone(), (*source).clone());
            }
        }
    }
    twins
}

/// A name under Unicode simple case folding: each character mapped on its own,
/// never to more than one — `ß` stays `ß` rather than becoming `ss`, which is
/// how the filesystems that fold at all compare names.
pub(super) fn fold(name: &str) -> String {
    name.chars().map(fold_char).collect()
}

/// One character's simple case fold.
///
/// Lowercasing is the fold for almost every character, and where lowercase
/// would be more than one character (`İ` is `i` and a combining dot) simple
/// folding leaves the character alone, as this does. The table is the rest:
/// characters that are already lowercase and still fold to another — final
/// sigma, long s, the micro sign and the Greek symbol variants.
pub(super) fn fold_char(c: char) -> char {
    match c {
        'ς' => 'σ',
        'ſ' => 's',
        'µ' => 'μ',
        '\u{1FBE}' => 'ι',
        'ϐ' => 'β',
        'ϑ' => 'θ',
        'ϕ' => 'φ',
        'ϖ' => 'π',
        'ϰ' => 'κ',
        'ϱ' => 'ρ',
        'ϵ' => 'ε',
        'ẛ' => 'ṡ',
        _ => {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(one), None) => one,
                _ => c,
            }
        }
    }
}

/// A directory's names, sorted, so the card lists a tree the way a listing
/// would and two plans of one tree come out in one order.
fn children(dir: &Path) -> Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| DfError::io(dir, e))? {
        names.push(entry.map_err(|e| DfError::io(dir, e))?.file_name());
    }
    names.sort();
    Ok(names)
}

/// Same size, and modified within [`MTIME_SLACK`] of each other.
fn quick_same(a: &Metadata, b: &Metadata) -> bool {
    if a.len() != b.len() {
        return false;
    }
    match (a.modified(), b.modified()) {
        (Ok(a), Ok(b)) => {
            let apart = a.duration_since(b).or_else(|_| b.duration_since(a));
            apart.is_ok_and(|apart| apart <= MTIME_SLACK)
        }
        // A filesystem with no modification times at all can only be
        // compared by size — or by contents, which is what the option is for.
        _ => true,
    }
}

/// Whether two files hash to the same SHA-256.
///
/// A file that cannot be read is not the same as anything: the pair is
/// called changed, and the copy that follows either succeeds or fails with
/// the real error, which is a better place to learn about it than a plan.
fn same_contents(a: &Path, b: &Path, stop: &dyn Fn() -> bool) -> Result<bool> {
    let (Some(a), Some(b)) = (digest(a, stop)?, digest(b, stop)?) else {
        return Ok(false);
    };
    Ok(a == b)
}

/// SHA-256 of a file, asking `stop` before every chunk. `None` when it cannot
/// be read.
fn digest(path: &Path, stop: &dyn Fn() -> bool) -> Result<Option<[u8; 32]>> {
    let Ok(mut file) = File::open(path) else {
        return Ok(None);
    };
    let mut hasher = crate::sha256::Sha256::new();
    let mut buf = vec![0u8; crate::ops::COPY_CHUNK];
    loop {
        if stop() {
            return Err(DfError::Cancelled);
        }
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Ok(None),
        }
    }
    Ok(Some(hasher.finish()))
}
