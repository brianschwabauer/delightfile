//! The executor: carry a plan out, durably, then prove it arrived.
//!
//! Failures are collected, never thrown. A sync of four thousand photos that
//! meets one unreadable file must still copy the other three thousand nine
//! hundred and ninety-nine, and the result card must be able to name the one —
//! so every copy error and every verify mismatch goes into the
//! [`SyncReport`], and the run carries on to the next path. Only a cancel
//! stops it early, and what landed before the cancel is real and stays.

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::ops::copy::{self, CopyOptions};
use crate::ops::COPY_CHUNK;
use crate::tasks::TaskCtx;
use crate::{DfError, Result};

use super::{Class, Item, Kind, SyncPlan, Verify};

/// What a sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub verify: Verify,
    /// Files and links written this run.
    pub copied: u64,
    pub copied_bytes: u64,
    /// Folders made.
    pub made: u64,
    /// Files and links read back on both sides and found the same.
    pub verified: u64,
    /// Each file the verify pass found wrong, and how.
    pub verify_failures: Vec<(PathBuf, String)>,
    /// Each path that could not be copied, and why.
    pub errors: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}

impl SyncReport {
    /// Everything that went wrong: the count the result card's title gives.
    pub fn problems(&self) -> usize {
        self.errors.len() + self.verify_failures.len()
    }
}

/// Carry out `plan`, then verify it.
///
/// Progress is honest from the first byte: the total is every byte to copy
/// plus every byte to read back — both sides of each verified file — and the
/// file count is every path to copy plus every path to verify. Pause and
/// cancel are the task's own, asked at every chunk of every copy and read.
pub fn execute(plan: &SyncPlan, verify: Verify, ctx: &TaskCtx) -> SyncReport {
    let mut run = Run {
        plan,
        ctx,
        report: SyncReport {
            verify,
            ..SyncReport::default()
        },
        copied: HashSet::new(),
        failed: HashSet::new(),
        failed_dirs: Vec::new(),
        buf: vec![0u8; COPY_CHUNK],
    };
    run.set_totals(verify);
    run.copy_all();
    #[cfg(test)]
    run_before_verify();
    if !run.report.cancelled {
        run.verify_all(verify);
    }
    run.report
}

struct Run<'a> {
    plan: &'a SyncPlan,
    ctx: &'a TaskCtx,
    report: SyncReport,
    /// The items this run wrote, by index: what [`Verify::Copied`] reads back.
    copied: HashSet<usize>,
    /// The items that failed, or were never tried because the folder they go
    /// in failed first: nothing to verify, already reported.
    failed: HashSet<usize>,
    /// Folders that could not be made, as `(root, rel)`: everything under one
    /// is skipped rather than failing once per file with the same error.
    failed_dirs: Vec<(usize, PathBuf)>,
    /// One read buffer for the whole verify pass, rather than a megabyte
    /// allocated per file of a card full of small ones.
    buf: Vec<u8>,
}

impl Run<'_> {
    fn copies(&self) -> impl Iterator<Item = (usize, &Item)> {
        self.plan
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item.class, Class::New | Class::Changed))
    }

    fn set_totals(&self, verify: Verify) {
        let (mut bytes, mut files) = (0, 0);
        for (_, item) in self.copies() {
            bytes += item.bytes;
            files += 1;
        }
        for item in self
            .plan
            .items
            .iter()
            .filter(|item| wants_verify(item, verify))
        {
            bytes += item.bytes * 2;
            files += 1;
        }
        self.ctx.set_total(bytes, files);
    }

    /// The copy pass: every new and changed path, parents before children.
    fn copy_all(&mut self) {
        let plan = self.plan;
        let mut made: Vec<(PathBuf, PathBuf)> = Vec::new();
        let copies: Vec<usize> = self.copies().map(|(index, _)| index).collect();
        for index in copies {
            let item = &plan.items[index];
            if self.under_failed_dir(item) {
                self.failed.insert(index);
                continue;
            }
            if self.ctx.checkpoint().is_err() {
                self.report.cancelled = true;
                break;
            }
            let (src, dst) = (plan.src_of(item), plan.dst_of(item));
            match self.copy_one(item, &src, &dst) {
                Ok(bytes) => {
                    if item.kind == Kind::Dir {
                        self.report.made += 1;
                        made.push((src, dst));
                    } else {
                        self.report.copied += 1;
                        self.report.copied_bytes += bytes;
                        self.copied.insert(index);
                    }
                }
                Err(DfError::Cancelled) => {
                    self.report.cancelled = true;
                    break;
                }
                Err(e) => {
                    self.failed.insert(index);
                    if item.kind == Kind::Dir {
                        self.failed_dirs.push((item.root, item.rel.clone()));
                    }
                    self.report.errors.push((dst, e.to_string()));
                }
            }
            self.ctx.advance(0, 1);
        }
        // A new folder's mode and times go on last, deepest first: making its
        // children bumped its time, and a read-only mode set before them would
        // have refused them.
        for (src, dst) in made.iter().rev() {
            if let Ok(meta) = std::fs::symlink_metadata(src) {
                copy::apply_mode(dst, &meta);
                copy::apply_times(dst, &meta);
            }
        }
    }

    fn under_failed_dir(&self, item: &Item) -> bool {
        self.failed_dirs
            .iter()
            .any(|(root, rel)| *root == item.root && item.rel.starts_with(rel))
    }

    /// Copy one path, and nothing under it — the walk order brings the
    /// children after. Returns the bytes moved.
    fn copy_one(&self, item: &Item, src: &Path, dst: &Path) -> Result<u64> {
        let options = CopyOptions {
            overwrite: item.class == Class::Changed,
            durable: true,
        };
        if item.class == Class::Changed {
            self.clear_the_way(item, dst)?;
        }
        match item.kind {
            Kind::Dir => {
                make_dir(dst)?;
                Ok(0)
            }
            Kind::File => copy::copy_file_with(src, dst, self.ctx, options),
            Kind::Symlink => {
                copy::copy_symlink(src, dst, options)?;
                Ok(0)
            }
            Kind::Special => Err(DfError::Op("not a file, folder or link".to_string())),
        }
    }

    /// A changed path whose destination is a different kind of thing.
    ///
    /// A file or a link where a folder goes is replaced, the way an overwrite
    /// replaces one file with another. A *folder* in the way of a file is
    /// refused: replacing it would throw away everything in it, and a sync
    /// that only adds and updates must never be how a folder disappears.
    fn clear_the_way(&self, item: &Item, dst: &Path) -> Result<()> {
        let Ok(meta) = std::fs::symlink_metadata(dst) else {
            return Ok(());
        };
        let theirs = Kind::of(&meta);
        if theirs == item.kind {
            return Ok(());
        }
        if theirs == Kind::Dir {
            return Err(DfError::Op(
                "a folder with that name is in the way".to_string(),
            ));
        }
        if item.kind == Kind::Dir {
            std::fs::remove_file(dst).map_err(|e| DfError::io(dst, e))?;
        }
        Ok(())
    }

    /// The verify pass: read both sides of each file back from the medium and
    /// compare digests; compare each link's target.
    fn verify_all(&mut self, verify: Verify) {
        let plan = self.plan;
        for (index, item) in plan.items.iter().enumerate() {
            let wanted = match verify {
                Verify::Copied => self.copied.contains(&index),
                Verify::Everything => {
                    wants_verify(item, verify)
                        && !self.failed.contains(&index)
                        && !self.under_failed_dir(item)
                }
            };
            if !wanted {
                continue;
            }
            if self.ctx.checkpoint().is_err() {
                self.report.cancelled = true;
                break;
            }
            let (src, dst) = (plan.src_of(item), plan.dst_of(item));
            let outcome = match item.kind {
                Kind::Symlink => same_target(&src, &dst),
                _ => self.same_bytes(&src, &dst),
            };
            match outcome {
                Ok(None) => self.report.verified += 1,
                Ok(Some(why)) => self.report.verify_failures.push((dst, why)),
                Err(DfError::Cancelled) => {
                    self.report.cancelled = true;
                    break;
                }
                Err(e) => self.report.verify_failures.push((dst, e.to_string())),
            }
            self.ctx.advance(0, 1);
        }
    }

    /// `None` when both files hash the same; otherwise what differs.
    fn same_bytes(&mut self, src: &Path, dst: &Path) -> Result<Option<String>> {
        let theirs = read_back(src, self.ctx, &mut self.buf)?;
        let ours = match read_back(dst, self.ctx, &mut self.buf) {
            Ok(digest) => digest,
            Err(DfError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some("missing at the destination".to_string()));
            }
            Err(e) => return Err(e),
        };
        Ok((theirs != ours).then(|| "contents differ from the source".to_string()))
    }
}

/// Whether [`Verify::Everything`] (or, as the upper bound the progress total
/// needs, [`Verify::Copied`]) would read `item` back.
fn wants_verify(item: &Item, verify: Verify) -> bool {
    let readable = matches!(item.kind, Kind::File | Kind::Symlink);
    match verify {
        Verify::Copied => readable && matches!(item.class, Class::New | Class::Changed),
        Verify::Everything => readable && item.class != Class::Extra,
    }
}

/// Make one new folder, and flush the name into its parent.
///
/// A folder that has appeared since the plan is taken as it is: making a
/// folder that is already there is the one idempotent thing in a sync.
fn make_dir(dst: &Path) -> Result<()> {
    match std::fs::create_dir(dst) {
        Ok(()) => copy::sync_parent(dst),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            match std::fs::symlink_metadata(dst) {
                Ok(meta) if meta.is_dir() => Ok(()),
                _ => Err(DfError::io(dst, e)),
            }
        }
        Err(e) => Err(DfError::io(dst, e)),
    }
}

/// `None` when two links point at the same text.
fn same_target(src: &Path, dst: &Path) -> Result<Option<String>> {
    let theirs = std::fs::read_link(src).map_err(|e| DfError::io(src, e))?;
    match std::fs::read_link(dst) {
        Ok(ours) if ours == theirs => Ok(None),
        Ok(_) => Ok(Some("the link points somewhere else".to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(Some("missing at the destination".to_string()))
        }
        Err(e) => Err(DfError::io(dst, e)),
    }
}

/// SHA-256 of a file as the medium holds it.
///
/// The page cache is dropped first. Straight after a copy every byte of both
/// files is still in RAM, and a verify that reads RAM proves the copy code,
/// not the disk — the card that lost a write, or the stick whose controller
/// lies about flushing, would read back perfect. `POSIX_FADV_DONTNEED` only
/// drops clean pages, which is why the copy flushed first; after it, a read
/// has to go to the device.
fn read_back(path: &Path, ctx: &TaskCtx, buf: &mut [u8]) -> Result<[u8; 32]> {
    let mut file = File::open(path).map_err(|e| DfError::io(path, e))?;
    forget_cached(&file, path);
    let mut hasher = crate::sha256::Sha256::new();
    loop {
        ctx.checkpoint()?;
        match file.read(buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
                ctx.advance(n as u64, 0);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(DfError::io(path, e)),
        }
    }
    Ok(hasher.finish())
}

/// Ask the kernel to drop a file's cached pages. Advisory: a filesystem that
/// ignores it (some FUSE mounts, where the server holds the cache) is logged
/// and read anyway, since a read through the cache still catches everything
/// but a lying medium.
fn forget_cached(file: &File, path: &Path) {
    use std::os::unix::io::AsRawFd;
    // One syscall on an fd we own, with no pointers involved.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    if rc != 0 {
        log::debug!(
            "{}: the page cache could not be dropped ({})",
            path.display(),
            std::io::Error::from_raw_os_error(rc)
        );
    }
}

#[cfg(test)]
thread_local! {
    /// Something to do between the copy pass and the verify pass, on this
    /// thread: how a test damages a file the copy has just written, which is
    /// the only way to prove the verify reads the disk rather than trusting
    /// the copy.
    static BEFORE_VERIFY: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `hook` once, after the next sync on this thread has copied and before
/// it verifies.
#[cfg(test)]
pub(crate) fn before_verify(hook: impl FnOnce() + 'static) {
    BEFORE_VERIFY.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
fn run_before_verify() {
    if let Some(hook) = BEFORE_VERIFY.with(|slot| slot.borrow_mut().take()) {
        hook();
    }
}
