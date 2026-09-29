//! Copy and move.
//!
//! Copy is reflink-first: on btrfs and XFS the `FICLONE` ioctl makes the
//! destination share the source's extents, so duplicating a 40 GB video is
//! instant and free (PLAN §5), and on macOS's APFS `fclonefileat` does the
//! same before the destination is opened. Everywhere else — ext4, tmpfs, a
//! different filesystem, a kernel that says no — it silently degrades to a
//! chunked read/write loop that reports progress and stops when cancelled.
//!
//! A copy can also be *durable* ([`CopyOptions::durable`]): every file is
//! flushed to the medium before it is renamed into place, and its directory
//! after, so "done" means on the disk rather than in the page cache. A paste
//! does not ask for it; a sync ([`crate::sync`]) always does.
//!
//! Every `user.*` extended attribute travels with the bytes — the file's tags
//! ([`crate::fs::tags`]) above all — copied after the contents and before the
//! mode, and on the paths that write through a temporary name (an overwrite,
//! a durable copy) before the rename out of it, so there no final name exists
//! without them. A fresh file a paste writes straight to its name gets them
//! just after its bytes, under that name. A destination that keeps no
//! attributes (a FAT card) still gets the file; the copy only notes that the
//! tags did not land ([`CopyStats::tags_dropped`]). A folder's own tags go
//! onto a folder the copy made, never onto one it merged into.
//!
//! Move is `rename(2)` first, because within one filesystem that is atomic and
//! costs nothing. Across filesystems `rename` returns `EXDEV` and there is no
//! choice but copy-then-delete — and the delete happens only after the copy has
//! been verified, because a move that loses the source is the one bug a file
//! manager may never have.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::platform::{self, errno};
use crate::tasks::TaskCtx;
use crate::{DfError, Result};

use super::{exists, file_name, is_real_dir, is_strict_ancestor_resolved, same_file};

/// Would putting `dst` inside `src` make the copy eat its own output?
///
/// Only a real directory can: a symlink is recreated in one syscall and
/// recurses into nothing. Resolved rather than lexical, because `dst` may reach
/// `src` through a symlinked parent — `cp -r a a/b` spelled so it does not look
/// like it.
fn copies_into_itself(src: &Path, dst: &Path) -> bool {
    is_real_dir(src) && is_strict_ancestor_resolved(src, dst)
}

/// How much of a file to move between two cancellation checkpoints.
///
/// 1 MiB is large enough that the syscall overhead is invisible next to the
/// disk (a 4 GiB file is 4096 read/write pairs, not a million) and small enough
/// that `d` on a paused copy responds within a few milliseconds even on a slow
/// spinning disk: at a pessimistic 30 MB/s one chunk is ~33 ms, well under the
/// ~100 ms at which a cancel stops feeling instant.
pub const COPY_CHUNK: usize = 1 << 20;

/// How many `.df-tmp-…` names to try before giving up.
///
/// The counter is process-wide and monotonic, so a collision means another
/// delightfile with the same pid — impossible — or a directory somebody is
/// filling with matching names. Sixteen tries is a formality either way.
const MAX_TEMP_ATTEMPTS: u32 = 16;

/// How a copy writes: whether it may replace what is in the way, and whether
/// it waits for the disk before calling a file done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyOptions {
    /// Replace an existing destination. See [`copy_tree`].
    pub overwrite: bool,
    /// `fsync` each written file before it is renamed into place, and the
    /// directory it lands in after — the same for a new directory or symlink,
    /// whose name is only as durable as the directory holding it.
    ///
    /// Off for a paste. The page cache is the kernel's promise, a paste is
    /// something the user watches land and can repeat, and an `fsync` per file
    /// on a thousand small ones is a paste that takes ten times as long for a
    /// guarantee nobody asked for. On for a sync, whose whole point is that the
    /// card can be pulled out afterwards: a copy that reports done while its
    /// bytes are still in RAM is the exact failure a sync exists to rule out,
    /// and a verify pass reading them back would read the same RAM and agree.
    pub durable: bool,
}

/// What a copy did, for the toast and for the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyStats {
    pub files: u64,
    pub bytes: u64,
    /// Sockets, fifos and device nodes: named here rather than copied, since a
    /// file manager cannot meaningfully duplicate them and silently skipping
    /// them would be a lie by omission.
    pub skipped: Vec<PathBuf>,
    /// A source carried tags and the destination would not hold them — a FAT
    /// card, a network mount without attributes. The copy itself succeeded;
    /// this is what the paste toast's "tags not kept on this drive" says.
    pub tags_dropped: bool,
}

impl CopyStats {
    fn merge(&mut self, other: CopyStats) {
        self.files += other.files;
        self.bytes += other.bytes;
        self.skipped.extend(other.skipped);
        self.tags_dropped |= other.tags_dropped;
    }
}

/// Total bytes and entries under `path`, for a progress bar that means
/// something before the copy starts.
///
/// Counts every entry the copy will create — files, symlinks *and* directories
/// — as one "file", because that is what the `312 / 4001` label counts. Symlink
/// bytes are not counted: recreating a link moves no data.
pub fn measure(path: &Path) -> Result<(u64, u64)> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| DfError::io(path, e))?;
    if meta.is_symlink() {
        return Ok((0, 1));
    }
    if meta.is_file() {
        return Ok((meta.len(), 1));
    }
    if !meta.is_dir() {
        return Ok((0, 0));
    }
    let mut bytes = 0;
    let mut files = 1; // the directory itself
    let entries = std::fs::read_dir(path).map_err(|e| DfError::io(path, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| DfError::io(path, e))?;
        let (b, f) = measure(&entry.path())?;
        bytes += b;
        files += f;
    }
    Ok((bytes, files))
}

/// Copy anything to anywhere: a file, a symlink (recreated, never followed) or
/// a whole tree.
///
/// `overwrite` decides what happens when `dst` already exists — the conflict
/// itself is resolved a layer up, in [`mod@super::paste`], because only the UI can
/// ask the user. On cancellation or failure, anything this call created is
/// removed again: a half-copied file is worse than no file, since it looks
/// complete in a listing.
pub fn copy_tree(src: &Path, dst: &Path, ctx: &TaskCtx, overwrite: bool) -> Result<CopyStats> {
    copy_tree_with(
        src,
        dst,
        ctx,
        CopyOptions {
            overwrite,
            durable: false,
        },
    )
}

/// [`copy_tree`], with the durability choice as well as the overwrite one.
pub fn copy_tree_with(
    src: &Path,
    dst: &Path,
    ctx: &TaskCtx,
    options: CopyOptions,
) -> Result<CopyStats> {
    if same_file(src, dst) {
        return Err(DfError::Op(format!(
            "{} and {} are the same file",
            src.display(),
            dst.display()
        )));
    }
    if copies_into_itself(src, dst) {
        return Err(DfError::Op(format!(
            "cannot copy {} into itself ({})",
            src.display(),
            dst.display()
        )));
    }
    let created_top = !exists(dst);
    match copy_entry(src, dst, ctx, options) {
        Ok(stats) => Ok(stats),
        Err(e) => {
            if created_top && exists(dst) {
                // Best effort: the operation already failed, and failing to
                // clean up must not replace the error the user needs to read.
                if let Err(cleanup) = super::delete::remove_tree_unchecked(dst) {
                    log::warn!("could not remove partial copy {}: {cleanup}", dst.display());
                }
            }
            Err(e)
        }
    }
}

fn copy_entry(src: &Path, dst: &Path, ctx: &TaskCtx, options: CopyOptions) -> Result<CopyStats> {
    ctx.checkpoint()?;
    let meta = std::fs::symlink_metadata(src).map_err(|e| DfError::io(src, e))?;

    if meta.is_symlink() {
        copy_symlink(src, dst, options)?;
        ctx.advance(0, 1);
        return Ok(CopyStats {
            files: 1,
            ..Default::default()
        });
    }
    if meta.is_dir() {
        return copy_dir(src, dst, ctx, options, &meta);
    }
    if meta.is_file() {
        let (bytes, tags_dropped) = copy_file(src, dst, ctx, options, &meta)?;
        // Bytes were reported chunk by chunk inside the copy; only the file
        // count is left to add.
        ctx.advance(0, 1);
        return Ok(CopyStats {
            files: 1,
            bytes,
            skipped: Vec::new(),
            tags_dropped,
        });
    }

    log::warn!(
        "skipping {}: not a regular file, directory or symlink",
        src.display()
    );
    Ok(CopyStats {
        skipped: vec![src.to_path_buf()],
        ..Default::default()
    })
}

/// Recreate a symlink, target text and all. Never followed: copying a link to
/// `/etc/passwd` copies the link, not the file — following it would be a
/// surprise with security consequences.
///
/// An overwrite removes whatever is in the way *whole*, directory included —
/// the caller has already decided that is what the user asked for.
pub(crate) fn copy_symlink(src: &Path, dst: &Path, options: CopyOptions) -> Result<()> {
    let target = std::fs::read_link(src).map_err(|e| DfError::io(src, e))?;
    if exists(dst) {
        if !options.overwrite {
            return Err(already_exists(dst));
        }
        super::delete::remove_tree_unchecked(dst)?;
    }
    platform::fs::symlink(&target, dst).map_err(|e| DfError::io(dst, e))?;
    if options.durable {
        sync_parent(dst)?;
    }
    Ok(())
}

fn copy_dir(
    src: &Path,
    dst: &Path,
    ctx: &TaskCtx,
    options: CopyOptions,
    meta: &std::fs::Metadata,
) -> Result<CopyStats> {
    // Whether the folder at `dst` is this copy's own, rather than one that
    // was there and is being merged into — which keeps its own tags.
    let made = if exists(dst) {
        if !options.overwrite {
            // Including a destination directory: merging into one the caller
            // did not know was there is a silent overwrite of every name that
            // collides inside it.
            return Err(already_exists(dst));
        }
        let dst_meta = std::fs::symlink_metadata(dst).map_err(|e| DfError::io(dst, e))?;
        if dst_meta.is_dir() {
            // An existing directory is merged into, which is what every file
            // manager does and what "overwrite" means for a folder.
            false
        } else {
            super::delete::remove_tree_unchecked(dst)?;
            std::fs::create_dir(dst).map_err(|e| DfError::io(dst, e))?;
            true
        }
    } else {
        std::fs::create_dir_all(dst).map_err(|e| DfError::io(dst, e))?;
        if options.durable {
            sync_parent(dst)?;
        }
        true
    };
    ctx.advance(0, 1);

    let mut stats = CopyStats {
        files: 1,
        ..Default::default()
    };
    let entries = std::fs::read_dir(src).map_err(|e| DfError::io(src, e))?;
    for entry in entries {
        ctx.checkpoint()?;
        let entry = entry.map_err(|e| DfError::io(src, e))?;
        let child_dst = dst.join(entry.file_name());
        stats.merge(copy_entry(&entry.path(), &child_dst, ctx, options)?);
    }

    // The folder's own tags, then mode and mtime last: creating the children
    // bumped the directory's mtime, so setting it before the recursion would
    // achieve nothing — and the attributes go before the mode, because a
    // read-only mode would refuse them. Only onto a folder this copy made: one
    // merged into is the destination's, and its tags are its own.
    if made {
        stats.tags_dropped |= crate::fs::tags::carry(src, dst);
    }
    apply_mode(dst, meta);
    apply_times(dst, meta);
    Ok(stats)
}

/// Copy one regular file's contents, then its `user.*` attributes, then its
/// mode and mtime. Returns the byte count actually moved, and whether the
/// file's tags were refused by the destination ([`CopyStats::tags_dropped`]).
///
/// The attributes go before the mode for the reason the mode goes before the
/// flush: a `0444` mode set first would refuse the `setxattr` that follows,
/// since a `user.*` attribute is written under the file's own write
/// permission.
///
/// An *overwrite* is written to a temporary file beside the destination and
/// renamed over it at the end. That costs one extra name in the directory and
/// buys the property that matters: a copy that fails, is cancelled, or is
/// killed half-way through leaves the destination exactly as it was, rather
/// than truncated to the length the copy reached. `rename(2)` over an existing
/// file is atomic, so there is no instant at which the destination is missing.
///
/// Durable ([`CopyOptions::durable`]), **every** file goes through a temporary
/// name, a new one as much as an overwrite. The mode and times are set while
/// it is still open and it is `fsync`ed after them, so the flush covers the
/// metadata as well as the bytes; only then is it renamed into place, and the
/// directory `fsync`ed, which is what makes the *name* survive a pulled card.
/// Written straight to its final name, a new file could be left by a crash or
/// a pulled card at full length with the source's date and bytes that never
/// reached the medium — which the next sync's size-and-date comparison would
/// call unchanged, and its toast "Already in sync". With the rename after the
/// flush, no final name exists without its bytes behind it. A flush that fails
/// is a failed copy, cleaned up like any other: a disk that cannot say the
/// bytes are down has not got them.
///
/// A paste is not durable and keeps writing a new file straight to its name:
/// it has no later comparison to fool, and the extra rename would be a cost for
/// nothing.
fn copy_file(
    src: &Path,
    dst: &Path,
    ctx: &TaskCtx,
    options: CopyOptions,
    meta: &std::fs::Metadata,
) -> Result<(u64, bool)> {
    let replacing = exists(dst);
    if replacing {
        if !options.overwrite {
            return Err(already_exists(dst));
        }
        if same_file(src, dst) {
            return Err(DfError::Op(format!(
                "refusing to overwrite {} with itself",
                dst.display()
            )));
        }
    }

    let mut reader = File::open(src).map_err(|e| DfError::io(src, e))?;
    let through_temp = replacing || options.durable;
    let write_path = if through_temp {
        temp_beside(dst)?
    } else {
        dst.to_path_buf()
    };
    let cloned = clone_first(&reader, &write_path)?;
    let mut writer = if cloned {
        // The clone is the whole file already, with the source's mode, so it
        // is opened only to be flushed.
        match File::open(&write_path) {
            Ok(file) => file,
            Err(e) => {
                let _ignored = std::fs::remove_file(&write_path);
                return Err(DfError::io(&write_path, e));
            }
        }
    } else if through_temp {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&write_path)
            .map_err(|e| DfError::io(&write_path, e))?
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(dst)
            .map_err(|e| DfError::io(dst, e))?
    };

    let written = if cloned {
        ctx.checkpoint().map(|()| {
            log::debug!("cloned {} → {}", src.display(), dst.display());
            ctx.advance(meta.len(), 0);
            meta.len()
        })
    } else {
        write_contents(&mut reader, &mut writer, src, &write_path, ctx, meta.len())
    };
    let result = written.and_then(|bytes| {
        let dropped = crate::fs::tags::carry(src, &write_path);
        apply_mode(&write_path, meta);
        apply_times(&write_path, meta);
        if options.durable {
            sync_file(&writer, &write_path)?;
        }
        Ok((bytes, dropped))
    });
    match result {
        Ok(copied) => {
            drop(writer);
            if write_path != dst {
                // A new file's name must still be free: something that took it
                // while the bytes were being written is not this copy's to
                // replace. The window is the rename's, as it always was the
                // `create`'s; a lock would not close it on a shared mount.
                if !options.overwrite && exists(dst) {
                    let _ignored = std::fs::remove_file(&write_path);
                    return Err(already_exists(dst));
                }
                if let Err(e) = std::fs::rename(&write_path, dst) {
                    let _ignored = std::fs::remove_file(&write_path);
                    return Err(DfError::io(dst, e));
                }
                #[cfg(test)]
                note(format!("rename {}", dst.display()));
            }
            if options.durable {
                sync_parent(dst)?;
            }
            Ok(copied)
        }
        Err(e) => {
            drop(writer);
            // A truncated file that looks like the real thing is the worst
            // outcome; remove it and let the error stand. When this was an
            // overwrite, the file being removed is the temporary one and the
            // destination has not been touched at all.
            if let Err(cleanup) = std::fs::remove_file(&write_path) {
                log::warn!(
                    "could not remove partial copy {}: {cleanup}",
                    write_path.display()
                );
            }
            Err(e)
        }
    }
}

/// Copy one regular file and nothing else: the entry point for a caller that
/// walks the tree itself and decides, file by file, what to carry — a sync
/// ([`crate::sync`]) copies only what differs, where [`copy_tree`] copies
/// everything under a path.
///
/// The same code as a file inside a tree copy — reflink first, chunked with a
/// checkpoint per chunk, an overwrite written beside the destination and
/// renamed over it — so the two cannot drift apart. Bytes are reported as they
/// move; the file count is the caller's to add.
pub(crate) fn copy_file_with(
    src: &Path,
    dst: &Path,
    ctx: &TaskCtx,
    options: CopyOptions,
) -> Result<u64> {
    ctx.checkpoint()?;
    let meta = std::fs::metadata(src).map_err(|e| DfError::io(src, e))?;
    copy_file(src, dst, ctx, options, &meta).map(|(bytes, _)| bytes)
}

/// `fsync(2)` one open file: its bytes and its metadata, down to the medium.
///
/// `File::sync_all` is exactly `fsync` on Linux, spelled without `unsafe`.
fn sync_file(file: &File, path: &Path) -> Result<()> {
    #[cfg(test)]
    {
        SYNCS.with(|c| c.set((c.get().0 + 1, c.get().1)));
        note(format!("fsync {}", path.display()));
    }
    file.sync_all().map_err(|e| DfError::io(path, e))
}

/// `fsync(2)` a file that is already written and closed, by opening it again:
/// how a file another program wrote — `rsync` on a download — is made as
/// durable as one this program wrote itself.
pub(crate) fn sync_path(path: &Path) -> Result<()> {
    let file = File::open(path).map_err(|e| DfError::io(path, e))?;
    sync_file(&file, path)
}

/// `fsync(2)` the directory `path` is in, so the name it was just given — a
/// rename, a `mkdir`, a `symlink` — is on the medium too.
pub(crate) fn sync_parent(path: &Path) -> Result<()> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => sync_dir(dir),
        _ => sync_dir(Path::new(".")),
    }
}

/// `fsync(2)` a directory: open it read-only, flush, close
/// ([`platform::fs::sync_dir`], which lets through a filesystem that cannot
/// flush one).
pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(test)]
    {
        SYNCS.with(|c| c.set((c.get().0, c.get().1 + 1)));
        note(format!("fsync-dir {}", dir.display()));
    }
    platform::fs::sync_dir(dir)
}

#[cfg(test)]
thread_local! {
    /// `(files, directories)` flushed on this thread, for the tests that prove
    /// a durable copy really does call `fsync` — the one effect of it no
    /// assertion on the files themselves can see.
    static SYNCS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// How many files and directories this thread has flushed so far.
#[cfg(test)]
pub(crate) fn syncs() -> (u64, u64) {
    SYNCS.with(|c| c.get())
}

#[cfg(test)]
thread_local! {
    /// The flushes and renames on this thread, in the order they happened:
    /// what proves a durable file is flushed *before* it gets its name.
    static EVENTS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn note(event: String) {
    EVENTS.with(|events| events.borrow_mut().push(event));
}

/// This thread's flushes and renames since the last call.
#[cfg(test)]
pub(crate) fn take_events() -> Vec<String> {
    EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

/// How every temporary name a copy makes begins. Public to the crate because
/// a sync reads it back: a name like this at a destination is a copy that was
/// killed before its rename, never anybody's file.
pub(crate) const TEMP_PREFIX: &str = ".df-tmp-";

/// A free `.df-tmp-…` name in the destination's own directory — the same
/// directory, so the final `rename` cannot cross a filesystem and fail.
fn temp_beside(dst: &Path) -> Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = dst.parent().unwrap_or(Path::new("."));
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let candidate = dir.join(format!("{TEMP_PREFIX}{}-{n}", std::process::id()));
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(DfError::Op(format!(
        "no free temporary name in {}",
        dir.display()
    )))
}

fn write_contents(
    reader: &mut File,
    writer: &mut File,
    src: &Path,
    dst: &Path,
    ctx: &TaskCtx,
    len: u64,
) -> Result<u64> {
    ctx.checkpoint()?;
    if reflink_enabled() && platform::fs::reflink(reader, writer) {
        log::debug!("reflinked {} → {}", src.display(), dst.display());
        ctx.advance(len, 0);
        return Ok(len);
    }

    let mut buf = vec![0u8; COPY_CHUNK];
    let mut total = 0u64;
    loop {
        ctx.checkpoint()?;
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            // A signal mid-read is not a failure; the retry is free.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(DfError::io(src, e)),
        };
        writer
            .write_all(&buf[..n])
            .map_err(|e| DfError::io(dst, e))?;
        total += n as u64;
        ctx.advance(n as u64, 0);
    }
    writer.flush().map_err(|e| DfError::io(dst, e))?;
    Ok(total)
}

/// Make `write_path` a clone of `reader`'s file, before anything is opened
/// there: macOS's `fclonefileat` on APFS
/// ([`platform::fs::clone_before_open`]), where a clone is a file made rather
/// than one filled. `false` wherever that is not how clones are made — Linux
/// clones into the open writer instead ([`platform::fs::reflink`]) — and when
/// the volume cannot, and under [`without_reflink`].
fn clone_first(reader: &File, write_path: &Path) -> Result<bool> {
    if !reflink_enabled() {
        return Ok(false);
    }
    platform::fs::clone_before_open(reader, write_path).map_err(|e| DfError::io(write_path, e))
}

/// Whether to try `FICLONE` (or macOS's clone) at all.
///
/// Always yes outside tests. Under `cfg(test)` it is a thread-local switch, so
/// the chunked fallback can be exercised deterministically on a machine whose
/// `$TMPDIR` happens to be btrfs or APFS — otherwise the fallback would only
/// ever be tested on ext4 and would rot on the developer's own laptop.
#[cfg(not(test))]
fn reflink_enabled() -> bool {
    true
}

#[cfg(test)]
thread_local! {
    static REFLINK_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

#[cfg(test)]
fn reflink_enabled() -> bool {
    REFLINK_ENABLED.with(|c| c.get())
}

/// Run `f` with `FICLONE` and macOS's clone disabled on this thread.
#[cfg(test)]
pub(crate) fn without_reflink<T>(f: impl FnOnce() -> T) -> T {
    REFLINK_ENABLED.with(|c| c.set(false));
    let out = f();
    REFLINK_ENABLED.with(|c| c.set(true));
    out
}

/// Copy the permission bits. A failure is logged, never fatal: a copy onto a
/// filesystem without Unix modes (FAT, some network mounts) still succeeded at
/// the part the user asked for.
pub(crate) fn apply_mode(path: &Path, meta: &std::fs::Metadata) {
    let mode = platform::meta::mode(meta);
    if let Err(e) = platform::fs::apply_mode(path, mode) {
        log::warn!("could not set mode on {}: {e}", path.display());
    }
}

/// Copy access and modification times. Same rule as [`apply_mode`]: advisory.
pub(crate) fn apply_times(path: &Path, meta: &std::fs::Metadata) {
    let mtime = meta.modified().ok();
    let atime = meta.accessed().ok();
    if let Err(e) = platform::fs::set_times(path, atime, mtime) {
        log::warn!("could not set times on {}: {e}", path.display());
    }
}

fn already_exists(dst: &Path) -> DfError {
    DfError::io(
        dst,
        std::io::Error::new(std::io::ErrorKind::AlreadyExists, "already exists"),
    )
}

/// Move `src` to `dst`: `rename(2)`, falling back to copy-verify-delete across
/// filesystems.
///
/// Returns what the copy did when the move had to be one — which is how a
/// cut pasted onto a FAT card learns its tags did not come along — and
/// nothing for a rename, which keeps the inode and every attribute on it.
pub fn move_path(src: &Path, dst: &Path, ctx: &TaskCtx, overwrite: bool) -> Result<CopyStats> {
    if same_file(src, dst) {
        return Err(DfError::Op(format!(
            "{} and {} are the same file",
            src.display(),
            dst.display()
        )));
    }
    if copies_into_itself(src, dst) {
        return Err(DfError::Op(format!(
            "cannot move {} into itself ({})",
            src.display(),
            dst.display()
        )));
    }
    // The other direction: replacing a directory that holds the source means
    // clearing the destination out of the way would delete the source with it,
    // leaving nothing at all behind. Refused here as well as in `plan_paste`,
    // because this is the call that does the deleting.
    if is_strict_ancestor_resolved(dst, src) {
        return Err(DfError::Op(format!(
            "cannot move {} onto {}: the destination contains it",
            src.display(),
            dst.display()
        )));
    }
    let replacing = exists(dst);
    if replacing && !overwrite {
        return Err(already_exists(dst));
    }

    // `rename(2)` replaces an existing file atomically, so the destination is
    // never missing and never half-written — the rename is tried *first*, even
    // when the user asked to overwrite. Only the shapes rename refuses (a
    // directory in the way of a file, a non-empty directory in the way of
    // anything) need the old destination removed, and then only once rename has
    // said so. Removing it up front destroyed the user's file whenever the
    // rename went on to fail for an unrelated reason — a read-only source
    // directory, a vanished mount — and there was nothing left to put back.
    match std::fs::rename(src, dst) {
        Ok(()) => return Ok(CopyStats::default()),
        Err(e) if errno::is_cross_device(&e) => {
            // Across filesystems the copy overwrites in place, temp-file and
            // all; an existing destination *directory* is merged into rather
            // than replaced, which leaves too much rather than too little.
            return move_cross_device(src, dst, ctx);
        }
        Err(e) if replacing && in_the_way(&e) => {}
        Err(e) => return Err(DfError::io(src, e)),
    }

    // The conflict dialog has already been through; this is the user's answer.
    super::delete::remove_tree_unchecked(dst)?;
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(CopyStats::default()),
        Err(e) if errno::is_cross_device(&e) => move_cross_device(src, dst, ctx),
        Err(e) => Err(DfError::io(src, e)),
    }
}

/// Does this `rename(2)` failure mean "the old destination is in the way", as
/// opposed to "this move cannot happen at all"?
///
/// Only asked when the destination is known to exist, which is what keeps
/// `ENOTDIR` from being read as "a component of the path is not a directory".
fn in_the_way(e: &std::io::Error) -> bool {
    // ENOTEMPTY/EEXIST: a directory with something in it. EISDIR: a
    // directory where the source is not one. ENOTDIR: the reverse.
    errno::is_not_empty(e) || errno::is_exists(e) || errno::is_dir(e) || errno::is_not_dir(e)
}

/// The cross-filesystem move: copy everything, prove it arrived, then delete
/// the source.
///
/// Split out and public so the `EXDEV` path is reachable from a test without
/// two filesystems to hand — the branch that deletes the user's data is not one
/// to leave untested because the fixture is inconvenient.
pub fn move_cross_device(src: &Path, dst: &Path, ctx: &TaskCtx) -> Result<CopyStats> {
    let stats = copy_tree(src, dst, ctx, true)?;
    verify_copy(src, dst)?;
    if !stats.skipped.is_empty() {
        // A fifo or device node could not be recreated, so the source is not
        // fully reproduced at the destination. Keeping the source is the only
        // non-destructive answer.
        return Err(DfError::Op(format!(
            "{}: {} special file(s) could not be moved; the original was kept",
            src.display(),
            stats.skipped.len()
        )));
    }
    super::delete::remove_tree(src, ctx)?;
    Ok(stats)
}

/// Prove that `dst` reproduces `src`: same kinds, same sizes, same link
/// targets, same names all the way down.
///
/// Not a checksum — reading every byte a second time would double the cost of
/// every cross-device move. Kind, size and structure catch the failures that
/// actually happen (a short write, a full disk, a skipped entry) before
/// anything is deleted.
pub fn verify_copy(src: &Path, dst: &Path) -> Result<()> {
    let s = std::fs::symlink_metadata(src).map_err(|e| DfError::io(src, e))?;
    let d = std::fs::symlink_metadata(dst).map_err(|e| DfError::io(dst, e))?;

    if s.is_symlink() != d.is_symlink() || s.is_dir() != d.is_dir() || s.is_file() != d.is_file() {
        return Err(mismatch(src, dst, "different kinds of file"));
    }
    if s.is_symlink() {
        let a = std::fs::read_link(src).map_err(|e| DfError::io(src, e))?;
        let b = std::fs::read_link(dst).map_err(|e| DfError::io(dst, e))?;
        if a != b {
            return Err(mismatch(src, dst, "symlink targets differ"));
        }
        return Ok(());
    }
    if s.is_file() {
        if s.len() != d.len() {
            return Err(mismatch(src, dst, "sizes differ"));
        }
        return Ok(());
    }
    if s.is_dir() {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(src).map_err(|e| DfError::io(src, e))? {
            let entry = entry.map_err(|e| DfError::io(src, e))?;
            names.push(entry.file_name());
        }
        for name in names {
            verify_copy(&src.join(&name), &dst.join(&name))?;
        }
    }
    Ok(())
}

fn mismatch(src: &Path, dst: &Path, why: &str) -> DfError {
    DfError::Op(format!(
        "copy of {} to {} could not be verified: {why}",
        src.display(),
        dst.display()
    ))
}

/// The destination path for copying `src` *into* directory `dir`.
pub fn dest_in(dir: &Path, src: &Path) -> Result<PathBuf> {
    Ok(dir.join(file_name(src)?))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::{gnarly_names, TempTree};
    use crate::tasks::{ProgressSink, TaskCtx, TaskFlags};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn ctx() -> TaskCtx {
        TaskCtx::detached()
    }

    /// A sink that cancels the task once a threshold of bytes has gone by, so
    /// "cancel mid-copy" is a deterministic assertion rather than a sleep.
    struct CancelAfter {
        flags: Arc<TaskFlags>,
        after: u64,
        seen: AtomicU64,
    }

    impl ProgressSink for CancelAfter {
        fn set_total(&self, _bytes: u64, _files: u64) {}
        fn advance(&self, bytes: u64, _files: u64) {
            let total = self.seen.fetch_add(bytes, Ordering::SeqCst) + bytes;
            if total >= self.after {
                self.flags.cancel();
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn copies_a_file_with_mode_and_mtime() {
        let t = TempTree::new("copy-file");
        let src = t.file("a.bin", &vec![7u8; 3 * COPY_CHUNK + 13]);
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o640)).unwrap();
        let src_meta = std::fs::metadata(&src).unwrap();

        let dst = t.join("b.bin");
        let stats = copy_tree(&src, &dst, &ctx(), false).unwrap();

        assert_eq!(stats.files, 1);
        assert_eq!(stats.bytes, 3 * COPY_CHUNK as u64 + 13);
        assert_eq!(std::fs::read(&src).unwrap(), std::fs::read(&dst).unwrap());
        let dst_meta = std::fs::metadata(&dst).unwrap();
        assert_eq!(dst_meta.permissions().mode() & 0o777, 0o640);
        assert_eq!(dst_meta.modified().unwrap(), src_meta.modified().unwrap());
    }

    #[test]
    fn copies_a_tree_of_gnarly_names() {
        let t = TempTree::new("copy-tree");
        let src = t.dir("src");
        for (i, name) in gnarly_names().iter().enumerate() {
            std::fs::write(src.join(name), format!("contents {i}")).unwrap();
        }
        std::fs::create_dir(src.join("nested dir")).unwrap();
        std::fs::write(src.join("nested dir/deep.txt"), b"deep").unwrap();

        let dst = t.join("dst");
        let stats = copy_tree(&src, &dst, &ctx(), false).unwrap();
        // Every name, plus both directories, plus the deep file.
        assert_eq!(stats.files as usize, gnarly_names().len() + 3);

        for name in gnarly_names() {
            assert!(dst.join(&name).is_file(), "missing {name:?}");
        }
        assert_eq!(
            std::fs::read(dst.join("nested dir/deep.txt")).unwrap(),
            b"deep"
        );
        verify_copy(&src, &dst).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn recreates_symlinks_without_following_them() {
        let t = TempTree::new("copy-symlink");
        let src = t.dir("src");
        std::fs::write(src.join("real.txt"), b"real").unwrap();
        std::os::unix::fs::symlink("real.txt", src.join("rel-link")).unwrap();
        std::os::unix::fs::symlink("/nowhere/at/all", src.join("broken")).unwrap();

        let dst = t.join("dst");
        copy_tree(&src, &dst, &ctx(), false).unwrap();

        let link = dst.join("rel-link");
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("real.txt"));
        let broken = dst.join("broken");
        assert!(std::fs::symlink_metadata(&broken).unwrap().is_symlink());
        assert_eq!(
            std::fs::read_link(&broken).unwrap(),
            Path::new("/nowhere/at/all")
        );
        assert!(!broken.exists(), "still broken, still copied");
    }

    #[test]
    fn cancel_mid_copy_removes_the_partial_file() {
        let t = TempTree::new("copy-cancel");
        // Several chunks, so there is a checkpoint to be cancelled at.
        let src = t.file("big.bin", &vec![1u8; 6 * COPY_CHUNK]);
        let dst = t.join("big-copy.bin");

        let flags = Arc::new(TaskFlags::new());
        let sink = Arc::new(CancelAfter {
            flags: Arc::clone(&flags),
            after: 0, // cancel at the first directory entry / first file
            seen: AtomicU64::new(0),
        });
        // Cancel before any bytes: the copy must not even start.
        flags.cancel();
        let ctx = TaskCtx::with_sink(flags, sink);
        let err = copy_tree(&src, &dst, &ctx, false).unwrap_err();
        assert!(matches!(err, DfError::Cancelled), "{err}");
        assert!(!exists(&dst), "no partial file may be left behind");
    }

    #[test]
    fn cancel_between_chunks_removes_the_partial_file() {
        let t = TempTree::new("copy-cancel-mid");
        // Six chunks, cancelled after the first: the loop must notice at the
        // next checkpoint and take the half-written file with it.
        let src = t.file("big.bin", &vec![3u8; 6 * COPY_CHUNK]);
        let dst = t.join("big-copy.bin");

        let flags = Arc::new(TaskFlags::new());
        let sink = Arc::new(CancelAfter {
            flags: Arc::clone(&flags),
            after: COPY_CHUNK as u64,
            seen: AtomicU64::new(0),
        });
        let ctx = TaskCtx::with_sink(flags, sink);
        let err = without_reflink(|| copy_tree(&src, &dst, &ctx, false)).unwrap_err();
        assert!(matches!(err, DfError::Cancelled), "{err}");
        assert!(!exists(&dst), "no partial file may be left behind");
        assert_eq!(
            std::fs::metadata(&src).unwrap().len(),
            6 * COPY_CHUNK as u64,
            "the source is untouched"
        );
    }

    #[test]
    fn cancel_part_way_through_a_tree_removes_what_it_created() {
        let t = TempTree::new("copy-cancel-tree");
        let src = t.dir("src");
        for i in 0..6 {
            std::fs::write(src.join(format!("f{i}.bin")), vec![2u8; COPY_CHUNK]).unwrap();
        }
        let dst = t.join("dst");

        let flags = Arc::new(TaskFlags::new());
        let sink = Arc::new(CancelAfter {
            flags: Arc::clone(&flags),
            after: COPY_CHUNK as u64,
            seen: AtomicU64::new(0),
        });
        let ctx = TaskCtx::with_sink(flags, sink);
        let err = copy_tree(&src, &dst, &ctx, false).unwrap_err();
        assert!(matches!(err, DfError::Cancelled), "{err}");
        assert!(!exists(&dst), "the destination this copy created is gone");
    }

    #[test]
    fn the_chunked_fallback_copies_identical_bytes() {
        let t = TempTree::new("copy-fallback");
        let body: Vec<u8> = (0..(2 * COPY_CHUNK + 7)).map(|i| (i % 251) as u8).collect();
        let src = t.file("a.bin", &body);
        let dst = t.join("b.bin");
        without_reflink(|| copy_tree(&src, &dst, &ctx(), false)).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), body);
    }

    #[test]
    fn reflink_failure_degrades_silently() {
        // A pipe is not a regular file, so FICLONE cannot possibly succeed —
        // this is the fallback path, asserted to be invisible to the caller.
        let t = TempTree::new("copy-reflink");
        let src = t.file("a.txt", b"hello reflink");
        let dst = t.join("b.txt");
        // /tmp is tmpfs on most machines and never supports FICLONE, so this
        // test usually exercises the fallback; on btrfs it exercises the
        // reflink. Either way the observable result is identical, which is the
        // entire contract.
        copy_tree(&src, &dst, &ctx(), false).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"hello reflink");
    }

    /// Unix only: the fixture opens a directory as a `File`, which Windows
    /// does not do, and Windows' reflink is `false` for everything anyway.
    #[cfg(unix)]
    #[test]
    fn reflink_of_a_non_regular_file_reports_failure() {
        let t = TempTree::new("reflink-neg");
        let src = t.file("a.txt", b"x");
        let a = File::open(&src).unwrap();
        let dir = File::open(t.path()).unwrap();
        assert!(
            !platform::fs::reflink(&a, &dir),
            "cloning into a directory cannot work"
        );
    }

    #[test]
    fn refuses_to_copy_a_directory_into_its_own_descendant() {
        let t = TempTree::new("copy-into-self");
        let src = t.dir("src/inner");
        let outer = t.join("src");
        let err = copy_tree(&outer, &src.join("copy"), &ctx(), false).unwrap_err();
        assert!(err.to_string().contains("into itself"), "{err}");
    }

    #[test]
    fn refuses_to_copy_a_directory_into_itself_through_a_symlink() {
        // BUG: the rail was lexical, so a destination spelled through a symlink
        // read as "somewhere else" and the copy recursed until the disk filled.
        let t = TempTree::new("copy-into-self-link");
        let src = t.dir("src");
        std::fs::write(src.join("a"), b"x").unwrap();
        let link = t.symlink(&src, "link");
        let err = copy_tree(&src, &link.join("copy"), &ctx(), false).unwrap_err();
        assert!(err.to_string().contains("into itself"), "{err}");

        // And the same for a move.
        let err = move_path(&src, &link.join("copy"), &ctx(), false).unwrap_err();
        assert!(err.to_string().contains("into itself"), "{err}");
    }

    #[test]
    fn a_symlink_may_be_copied_into_the_directory_it_points_at() {
        // The rail is about a *directory* eating its own output; a link is one
        // syscall and recurses into nothing.
        let t = TempTree::new("copy-link-into-target");
        let dir = t.dir("dir");
        let link = t.symlink(&dir, "link");
        copy_tree(&link, &dir.join("copied-link"), &ctx(), false).unwrap();
        assert!(std::fs::symlink_metadata(dir.join("copied-link"))
            .unwrap()
            .is_symlink());
    }

    #[test]
    fn refuses_to_copy_a_file_over_itself() {
        let t = TempTree::new("copy-self");
        let src = t.file("a.txt", b"x");
        let err = copy_tree(&src, &src, &ctx(), true).unwrap_err();
        assert!(err.to_string().contains("same file"), "{err}");
    }

    #[test]
    fn will_not_clobber_without_overwrite() {
        let t = TempTree::new("copy-clobber");
        let src = t.file("a.txt", b"new");
        let dst = t.file("b.txt", b"old");
        let err = copy_tree(&src, &dst, &ctx(), false).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&dst).unwrap(), b"old");

        copy_tree(&src, &dst, &ctx(), true).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"new");
    }

    #[test]
    fn a_failed_overwrite_leaves_the_old_file_whole() {
        let t = TempTree::new("copy-overwrite-fail");
        let src = t.file("src.bin", &vec![9u8; 6 * COPY_CHUNK]);
        let dst = t.file("dst.bin", b"the previous contents");

        let flags = Arc::new(TaskFlags::new());
        let sink = Arc::new(CancelAfter {
            flags: Arc::clone(&flags),
            after: COPY_CHUNK as u64,
            seen: AtomicU64::new(0),
        });
        let ctx = TaskCtx::with_sink(flags, sink);
        let err = without_reflink(|| copy_tree(&src, &dst, &ctx, true)).unwrap_err();
        assert!(matches!(err, DfError::Cancelled), "{err}");
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            b"the previous contents",
            "an interrupted overwrite must not truncate the destination"
        );
        // And no temporary file is left lying about.
        let strays: Vec<_> = std::fs::read_dir(t.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".df-tmp-"))
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
    }

    #[test]
    fn a_failed_copy_does_not_delete_a_pre_existing_destination() {
        let t = TempTree::new("copy-keep-dst");
        let src = t.dir("src");
        std::fs::write(src.join("f"), b"x").unwrap();
        let dst = t.dir("dst");
        std::fs::write(dst.join("keep-me"), b"precious").unwrap();

        let flags = Arc::new(TaskFlags::new());
        flags.cancel();
        let ctx = TaskCtx::with_sink(Arc::clone(&flags), Arc::new(crate::tasks::NullSink));
        assert!(copy_tree(&src, &dst, &ctx, true).is_err());
        assert_eq!(std::fs::read(dst.join("keep-me")).unwrap(), b"precious");
    }

    #[test]
    fn a_directory_is_not_silently_merged_into_without_overwrite() {
        // BUG: an existing destination *directory* was merged into whatever the
        // caller said, so a destination that appeared between the plan and the
        // execute quietly overwrote every colliding name inside it — and the
        // journal then recorded the whole merged directory as a fresh copy.
        let t = TempTree::new("copy-merge");
        let src = t.dir("src");
        std::fs::write(src.join("a"), b"new").unwrap();
        let dst = t.dir("dst");
        std::fs::write(dst.join("a"), b"old").unwrap();

        let err = copy_tree(&src, &dst, &ctx(), false).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"old");

        // With overwrite it still merges, which is what the dialog's answer
        // means for a folder.
        copy_tree(&src, &dst, &ctx(), true).unwrap();
        assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn a_durable_copy_flushes_every_file_and_every_directory_it_names() {
        let t = TempTree::new("copy-durable");
        let src = t.dir("src");
        std::fs::write(src.join("a"), b"aaa").unwrap();
        std::fs::create_dir(src.join("d")).unwrap();
        std::fs::write(src.join("d/b"), b"bbb").unwrap();
        std::os::unix::fs::symlink("a", src.join("link")).unwrap();

        let before = syncs();
        copy_tree(&src, &t.join("plain"), &ctx(), false).unwrap();
        assert_eq!(syncs(), before, "a paste waits for nobody");

        let options = CopyOptions {
            overwrite: false,
            durable: true,
        };
        copy_tree_with(&src, &t.join("durable"), &ctx(), options).unwrap();
        let (files, dirs) = syncs();
        assert_eq!(files - before.0, 2, "each of the two files, once");
        // Two directories made (the top and `d`), two files and a link named:
        // five names, each flushed in the directory that holds it.
        assert_eq!(dirs - before.1, 5);
        assert_eq!(std::fs::read(t.join("durable/d/b")).unwrap(), b"bbb");
    }

    #[test]
    fn a_durable_new_file_has_no_name_until_its_bytes_are_flushed() {
        let t = TempTree::new("copy-durable-new");
        let src = t.file("src.bin", &vec![6u8; COPY_CHUNK + 9]);
        let dst = t.join("out/new.bin");
        std::fs::create_dir(t.join("out")).unwrap();
        take_events();
        let options = CopyOptions {
            overwrite: false,
            durable: true,
        };
        without_reflink(|| copy_file_with(&src, &dst, &ctx(), options)).unwrap();
        let events = take_events();
        assert_eq!(events.len(), 3, "{events:?}");
        // The bytes are flushed under a temporary name beside the file…
        let flushed = events[0].strip_prefix("fsync ").expect("the file first");
        let flushed = Path::new(flushed);
        assert_eq!(flushed.parent(), dst.parent());
        assert!(
            flushed
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".df-tmp-"),
            "{events:?}"
        );
        // …then it is named, then the name is flushed.
        assert_eq!(events[1], format!("rename {}", dst.display()));
        assert_eq!(events[2], format!("fsync-dir {}", t.join("out").display()));
        assert_eq!(std::fs::read(&dst).unwrap(), std::fs::read(&src).unwrap());
        assert!(!flushed.exists());

        // A paste writes a new file straight to its name, and waits for nothing.
        let pasted = t.join("out/pasted.bin");
        copy_tree(&src, &pasted, &ctx(), false).unwrap();
        assert!(take_events().is_empty());
    }

    #[test]
    fn a_durable_overwrite_flushes_before_the_rename_and_after_it() {
        let t = TempTree::new("copy-durable-overwrite");
        let src = t.file("src.bin", &vec![4u8; COPY_CHUNK + 5]);
        let dst = t.file("dst.bin", b"old");
        let before = syncs();
        let options = CopyOptions {
            overwrite: true,
            durable: true,
        };
        let bytes = without_reflink(|| copy_file_with(&src, &dst, &ctx(), options)).unwrap();
        assert_eq!(bytes, COPY_CHUNK as u64 + 5);
        assert_eq!(syncs(), (before.0 + 1, before.1 + 1));
        assert_eq!(std::fs::read(&dst).unwrap(), std::fs::read(&src).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn measure_counts_bytes_and_entries() {
        let t = TempTree::new("measure");
        let src = t.dir("src");
        std::fs::write(src.join("a"), b"12345").unwrap();
        std::fs::write(src.join("b"), b"123").unwrap();
        std::os::unix::fs::symlink("a", src.join("l")).unwrap();
        let (bytes, files) = measure(&src).unwrap();
        assert_eq!(bytes, 8);
        assert_eq!(files, 4, "two files, one symlink, one directory");
    }

    #[test]
    fn move_uses_rename_within_a_filesystem() {
        let t = TempTree::new("move");
        let src = t.file("a.txt", b"data");
        let dst = t.join("sub/b.txt");
        std::fs::create_dir(t.join("sub")).unwrap();
        move_path(&src, &dst, &ctx(), false).unwrap();
        assert!(!exists(&src));
        assert_eq!(std::fs::read(&dst).unwrap(), b"data");
    }

    #[test]
    fn refuses_to_move_something_onto_a_directory_that_contains_it() {
        // BUG: the destination was removed to make room for the rename, and
        // when the destination *contained* the source that removal took the
        // source with it — both gone, and the rename then failed with ENOENT.
        // `plan_paste` refuses this a layer up; the rail belongs here too,
        // because `move_path` is the public call that does the damage.
        let t = TempTree::new("move-onto-container");
        let inner = t.dir("parent/inner");
        let src = t.file("parent/inner/thing", b"the only copy");
        std::fs::write(inner.join("neighbour"), b"also precious").unwrap();

        let err = move_path(&src, &inner, &ctx(), true).unwrap_err();
        assert!(err.to_string().contains("contains it"), "{err}");
        assert_eq!(std::fs::read(&src).unwrap(), b"the only copy");
        assert!(inner.join("neighbour").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_move_that_cannot_happen_leaves_the_destination_alone() {
        // BUG: an overwriting move deleted the old destination *before* trying
        // the rename, so a rename that then failed left the user with neither
        // file — the source still in place and the destination annihilated.
        let t = TempTree::new("move-overwrite-fail");
        let src_dir = t.dir("src");
        let src = t.file("src/a.txt", b"the source");
        let dst = t.file("dst/a.txt", b"the precious destination");

        // A source directory nothing may unlink from: `rename` needs write
        // permission on it, so the move is impossible.
        std::fs::set_permissions(&src_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = move_path(&src, &dst, &ctx(), true).unwrap_err();
        std::fs::set_permissions(&src_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(matches!(err, DfError::Io { .. }), "{err}");
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            b"the precious destination",
            "a move that could not happen must not have destroyed the destination"
        );
        assert_eq!(std::fs::read(&src).unwrap(), b"the source");
    }

    #[test]
    fn an_overwriting_move_still_replaces_a_directory() {
        // The other half of the rail above: when the rename genuinely is
        // blocked by the old destination, it still goes.
        let t = TempTree::new("move-overwrite-dir");
        let src = t.dir("src");
        std::fs::write(src.join("new"), b"new").unwrap();
        let dst = t.dir("dst");
        std::fs::write(dst.join("old"), b"old").unwrap();

        move_path(&src, &dst, &ctx(), true).unwrap();
        assert!(!exists(&src));
        assert!(dst.join("new").is_file());
        assert!(
            !exists(&dst.join("old")),
            "the old destination was replaced"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cross_device_move_copies_verifies_then_deletes() {
        let t = TempTree::new("move-xdev");
        let src = t.dir("src");
        std::fs::write(src.join("a"), b"aaa").unwrap();
        std::fs::create_dir(src.join("d")).unwrap();
        std::fs::write(src.join("d/b"), b"bbb").unwrap();
        std::os::unix::fs::symlink("../a", src.join("d/link")).unwrap();

        let dst = t.join("dst");
        move_cross_device(&src, &dst, &ctx()).unwrap();

        assert!(!exists(&src), "the source is gone only after the copy");
        assert_eq!(std::fs::read(dst.join("d/b")).unwrap(), b"bbb");
        assert_eq!(
            std::fs::read_link(dst.join("d/link")).unwrap(),
            Path::new("../a")
        );
    }

    // ── Tags and the other `user.*` attributes ─────────────────────────────

    use crate::fs::tags;

    fn tagged(path: &Path, names: &[&str]) {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        tags::write(path, &names).unwrap();
    }

    /// A tree whose files can hold attributes, or `None` on a `$TMPDIR` that
    /// cannot (said on stderr, so the skip is visible).
    fn tag_tree(label: &str) -> Option<TempTree> {
        let t = TempTree::new(label);
        let probe = t.file("probe", b"");
        let ok = tags::supported_here(&probe);
        std::fs::remove_file(probe).unwrap();
        ok.then_some(t)
    }

    /// Tags travel with every shape of copy: a new file, a file written over
    /// another through its temporary name, a durable copy, a folder and what
    /// is inside it — and a read-only file, whose mode would refuse them if it
    /// were set first.
    #[cfg(unix)]
    #[test]
    fn a_copy_carries_the_tags() {
        let Some(t) = tag_tree("copy-tags") else {
            return;
        };
        let src = t.dir("src");
        let file = t.file("src/notes.txt", b"x");
        tagged(&src, &["blue"]);
        tagged(&file, &["red", "invoice 2026"]);
        let locked = t.file("src/locked.txt", b"x");
        tagged(&locked, &["green"]);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o444)).unwrap();

        let dst = t.join("dst");
        let stats = without_reflink(|| copy_tree(&src, &dst, &ctx(), false)).unwrap();
        assert!(!stats.tags_dropped);
        assert_eq!(tags::read(&dst), ["blue"]);
        assert_eq!(tags::read(&dst.join("notes.txt")), ["red", "invoice 2026"]);
        assert_eq!(tags::read(&dst.join("locked.txt")), ["green"]);

        // Written over an existing file, by way of `.df-tmp-…`.
        let over = t.file("over.txt", b"old");
        tagged(&over, &["stale"]);
        copy_tree(&file, &over, &ctx(), true).unwrap();
        assert_eq!(tags::read(&over), ["red", "invoice 2026"]);

        // Durable, as a sync copies.
        let durable = t.join("durable.txt");
        let options = CopyOptions {
            overwrite: false,
            durable: true,
        };
        copy_file_with(&file, &durable, &ctx(), options).unwrap();
        assert_eq!(tags::read(&durable), ["red", "invoice 2026"]);
    }

    /// A folder pasted over a folder is merged into it, and the folder merged
    /// into keeps its own tags — what lands inside it brings theirs. A folder
    /// that replaces a file in the way is the copy's own, and takes the
    /// source folder's.
    #[test]
    fn a_merged_folder_keeps_its_own_tags() {
        let Some(t) = tag_tree("copy-tags-merge") else {
            return;
        };
        let src = t.dir("src");
        let inside = t.file("src/new.txt", b"x");
        tagged(&src, &["holiday"]);
        tagged(&inside, &["red"]);
        let dst = t.dir("dst");
        tagged(&dst, &["archive"]);

        copy_tree(&src, &dst, &ctx(), true).unwrap();
        assert_eq!(tags::read(&dst), ["archive"], "the destination's folder");
        assert_eq!(tags::read(&dst.join("new.txt")), ["red"]);

        let file_in_the_way = t.file("was-a-file", b"x");
        tagged(&file_in_the_way, &["stale"]);
        copy_tree(&src, &file_in_the_way, &ctx(), true).unwrap();
        assert_eq!(tags::read(&file_in_the_way), ["holiday"]);
    }
    /// A move across drives is a copy and a delete, and the tags are in the
    /// copy — the one kind of move that does not keep the inode.
    #[test]
    fn a_cross_device_move_keeps_the_tags() {
        let Some(t) = tag_tree("move-xdev-tags") else {
            return;
        };
        let src = t.dir("src");
        let inner = t.file("src/d/b.txt", b"b");
        tagged(&src, &["work"]);
        tagged(&inner, &["red"]);
        let dst = t.join("dst");
        let stats = move_cross_device(&src, &dst, &ctx()).unwrap();
        assert!(!stats.tags_dropped);
        assert!(!exists(&src));
        assert_eq!(tags::read(&dst), ["work"]);
        assert_eq!(tags::read(&dst.join("d/b.txt")), ["red"]);

        // A plain rename keeps them for nothing, and has no copy to report.
        let renamed = t.join("renamed");
        let stats = move_path(&dst, &renamed, &ctx(), false).unwrap();
        assert_eq!(stats, CopyStats::default());
        assert_eq!(tags::read(&renamed.join("d/b.txt")), ["red"]);
    }

    /// A drive that holds no attributes (a FAT card) still gets every file;
    /// the copy only says the tags did not come along — and only when there
    /// were tags to lose.
    #[test]
    fn a_drive_that_refuses_tags_still_gets_the_files() {
        let Some(t) = tag_tree("copy-tags-refused") else {
            return;
        };
        let src = t.dir("src");
        let file = t.file("src/a.txt", b"aaa");
        tagged(&file, &["red"]);
        let plain = t.file("plain.txt", b"x");

        // A drive that keeps no attributes is another volume, which no
        // clone reaches: macOS's clone would carry them past the refusal.
        let card = |f: &dyn Fn() -> Result<CopyStats>| without_reflink(|| tags::refusing(f));
        let dst = t.join("card");
        let stats = card(&|| copy_tree(&src, &dst, &ctx(), false)).unwrap();
        assert!(stats.tags_dropped);
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"aaa");
        assert!(tags::read(&dst.join("a.txt")).is_empty());

        let stats = card(&|| copy_tree(&plain, &t.join("p.txt"), &ctx(), false));
        assert!(
            !stats.unwrap().tags_dropped,
            "nothing to lose, nothing lost"
        );

        // A move onto it keeps nothing back either: the source goes, as it
        // would with any other move, and the report says what was not kept.
        let moved = t.join("moved");
        let stats = card(&|| move_cross_device(&src, &moved, &ctx())).unwrap();
        assert!(stats.tags_dropped);
        assert!(!exists(&src));
        assert_eq!(std::fs::read(moved.join("a.txt")).unwrap(), b"aaa");
    }

    #[test]
    fn cross_device_move_keeps_the_source_when_the_copy_fails() {
        let t = TempTree::new("move-xdev-fail");
        let src = t.dir("src");
        std::fs::write(src.join("a"), b"aaa").unwrap();
        let dst = t.join("dst");

        let flags = Arc::new(TaskFlags::new());
        flags.cancel();
        let ctx = TaskCtx::with_sink(flags, Arc::new(crate::tasks::NullSink));
        assert!(move_cross_device(&src, &dst, &ctx).is_err());
        assert!(exists(&src.join("a")), "nothing is deleted on failure");
    }

    #[test]
    fn verify_catches_a_truncated_copy() {
        let t = TempTree::new("verify");
        let src = t.file("src/a", b"12345");
        let dst = t.file("dst/a", b"123");
        assert!(verify_copy(&src, &dst).is_err());
        std::fs::write(&dst, b"12345").unwrap();
        verify_copy(&src, &dst).unwrap();
    }

    #[test]
    fn verify_catches_a_missing_entry() {
        let t = TempTree::new("verify-missing");
        let src = t.dir("src");
        std::fs::write(src.join("a"), b"x").unwrap();
        std::fs::write(src.join("b"), b"y").unwrap();
        let dst = t.dir("dst");
        std::fs::write(dst.join("a"), b"x").unwrap();
        assert!(verify_copy(&src, &dst).is_err());
    }
}
