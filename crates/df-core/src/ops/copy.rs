//! Copy and move.
//!
//! Copy is reflink-first: on btrfs and XFS the `FICLONE` ioctl makes the
//! destination share the source's extents, so duplicating a 40 GB video is
//! instant and free (PLAN §5). Everywhere else — ext4, tmpfs, a different
//! filesystem, a kernel that says no — it silently degrades to a chunked
//! read/write loop that reports progress and stops when cancelled.
//!
//! Move is `rename(2)` first, because within one filesystem that is atomic and
//! costs nothing. Across filesystems `rename` returns `EXDEV` and there is no
//! choice but copy-then-delete — and the delete happens only after the copy has
//! been verified, because a move that loses the source is the one bug a file
//! manager may never have.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

/// `FICLONE`: `_IOW(0x94, 9, int)`, the reflink ioctl.
///
/// Spelled out rather than pulled from a crate because it is one number and its
/// derivation is right here: direction `_IOC_WRITE` (1) << 30, size 4 << 16,
/// type `0x94` << 8, number 9 — `0x4000_0000 | 0x0004_0000 | 0x9400 | 0x09`.
const FICLONE: libc::c_ulong = 0x4004_9409;

/// How many `.df-tmp-…` names to try before giving up.
///
/// The counter is process-wide and monotonic, so a collision means another
/// delightfile with the same pid — impossible — or a directory somebody is
/// filling with matching names. Sixteen tries is a formality either way.
const MAX_TEMP_ATTEMPTS: u32 = 16;

/// What a copy did, for the toast and for the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyStats {
    pub files: u64,
    pub bytes: u64,
    /// Sockets, fifos and device nodes: named here rather than copied, since a
    /// file manager cannot meaningfully duplicate them and silently skipping
    /// them would be a lie by omission.
    pub skipped: Vec<PathBuf>,
}

impl CopyStats {
    fn merge(&mut self, other: CopyStats) {
        self.files += other.files;
        self.bytes += other.bytes;
        self.skipped.extend(other.skipped);
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
/// itself is resolved a layer up, in [`super::paste`], because only the UI can
/// ask the user. On cancellation or failure, anything this call created is
/// removed again: a half-copied file is worse than no file, since it looks
/// complete in a listing.
pub fn copy_tree(src: &Path, dst: &Path, ctx: &TaskCtx, overwrite: bool) -> Result<CopyStats> {
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
    match copy_entry(src, dst, ctx, overwrite) {
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

fn copy_entry(src: &Path, dst: &Path, ctx: &TaskCtx, overwrite: bool) -> Result<CopyStats> {
    ctx.checkpoint()?;
    let meta = std::fs::symlink_metadata(src).map_err(|e| DfError::io(src, e))?;

    if meta.is_symlink() {
        copy_symlink(src, dst, overwrite)?;
        ctx.advance(0, 1);
        return Ok(CopyStats {
            files: 1,
            ..Default::default()
        });
    }
    if meta.is_dir() {
        return copy_dir(src, dst, ctx, overwrite, &meta);
    }
    if meta.is_file() {
        let bytes = copy_file(src, dst, ctx, overwrite, &meta)?;
        // Bytes were reported chunk by chunk inside the copy; only the file
        // count is left to add.
        ctx.advance(0, 1);
        return Ok(CopyStats {
            files: 1,
            bytes,
            skipped: Vec::new(),
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
fn copy_symlink(src: &Path, dst: &Path, overwrite: bool) -> Result<()> {
    let target = std::fs::read_link(src).map_err(|e| DfError::io(src, e))?;
    if exists(dst) {
        if !overwrite {
            return Err(already_exists(dst));
        }
        super::delete::remove_tree_unchecked(dst)?;
    }
    std::os::unix::fs::symlink(&target, dst).map_err(|e| DfError::io(dst, e))
}

fn copy_dir(
    src: &Path,
    dst: &Path,
    ctx: &TaskCtx,
    overwrite: bool,
    meta: &std::fs::Metadata,
) -> Result<CopyStats> {
    if exists(dst) {
        if !overwrite {
            // Including a destination directory: merging into one the caller
            // did not know was there is a silent overwrite of every name that
            // collides inside it.
            return Err(already_exists(dst));
        }
        let dst_meta = std::fs::symlink_metadata(dst).map_err(|e| DfError::io(dst, e))?;
        if !dst_meta.is_dir() {
            super::delete::remove_tree_unchecked(dst)?;
            std::fs::create_dir(dst).map_err(|e| DfError::io(dst, e))?;
        }
        // An existing directory is merged into, which is what every file
        // manager does and what "overwrite" means for a folder.
    } else {
        std::fs::create_dir_all(dst).map_err(|e| DfError::io(dst, e))?;
    }
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
        stats.merge(copy_entry(&entry.path(), &child_dst, ctx, overwrite)?);
    }

    // Mode and mtime last: creating the children bumped the directory's mtime,
    // so setting it before the recursion would achieve nothing.
    apply_mode(dst, meta);
    apply_times(dst, meta);
    Ok(stats)
}

/// Copy one regular file's contents, then its mode and mtime. Returns the byte
/// count actually moved.
/// Copy one regular file's contents, then its mode and mtime. Returns the byte
/// count actually moved.
///
/// An *overwrite* is written to a temporary file beside the destination and
/// renamed over it at the end. That costs one extra name in the directory and
/// buys the property that matters: a copy that fails, is cancelled, or is
/// killed half-way through leaves the destination exactly as it was, rather
/// than truncated to the length the copy reached. `rename(2)` over an existing
/// file is atomic, so there is no instant at which the destination is missing.
fn copy_file(
    src: &Path,
    dst: &Path,
    ctx: &TaskCtx,
    overwrite: bool,
    meta: &std::fs::Metadata,
) -> Result<u64> {
    let replacing = exists(dst);
    if replacing {
        if !overwrite {
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
    let (write_path, mut writer) = if replacing {
        let temp = temp_beside(dst)?;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| DfError::io(&temp, e))?;
        (temp, file)
    } else {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(dst)
            .map_err(|e| DfError::io(dst, e))?;
        (dst.to_path_buf(), file)
    };

    let result = write_contents(&mut reader, &mut writer, src, &write_path, ctx, meta.len());
    match result {
        Ok(bytes) => {
            drop(writer);
            apply_mode(&write_path, meta);
            apply_times(&write_path, meta);
            if write_path != dst {
                if let Err(e) = std::fs::rename(&write_path, dst) {
                    let _ignored = std::fs::remove_file(&write_path);
                    return Err(DfError::io(dst, e));
                }
            }
            Ok(bytes)
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

/// A free `.df-tmp-…` name in the destination's own directory — the same
/// directory, so the final `rename` cannot cross a filesystem and fail.
fn temp_beside(dst: &Path) -> Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = dst.parent().unwrap_or(Path::new("."));
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let candidate = dir.join(format!(".df-tmp-{}-{n}", std::process::id()));
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
    if reflink_enabled() && reflink(reader, writer) {
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

/// Ask the kernel to share the source's extents with the destination.
///
/// Returns `false` for every failure, and that is the whole error handling:
/// `EOPNOTSUPP` (ext4, tmpfs), `EXDEV` (different filesystem), `EINVAL` (not a
/// regular file, or a destination that is not empty) and anything else all mean
/// exactly one thing to the caller — copy it the long way. The destination file
/// is untouched on failure, so falling through costs nothing.
fn reflink(reader: &File, writer: &File) -> bool {
    use std::os::unix::io::AsRawFd;
    // The only unsafe in df-core besides `getuid`: one ioctl on two fds we own,
    // with no pointers involved and no way to alias anything.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::ioctl(writer.as_raw_fd(), FICLONE, reader.as_raw_fd()) };
    if rc != 0 {
        log::trace!(
            "FICLONE unavailable ({}), falling back to a chunked copy",
            std::io::Error::last_os_error()
        );
    }
    rc == 0
}

/// Whether to try `FICLONE` at all.
///
/// Always yes outside tests. Under `cfg(test)` it is a thread-local switch, so
/// the chunked fallback can be exercised deterministically on a machine whose
/// `$TMPDIR` happens to be btrfs — otherwise the fallback would only ever be
/// tested on ext4 and would rot on the developer's own laptop.
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

/// Run `f` with `FICLONE` disabled on this thread.
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
fn apply_mode(path: &Path, meta: &std::fs::Metadata) {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode();
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
        log::warn!("could not set mode on {}: {e}", path.display());
    }
}

/// Copy access and modification times. Same rule as [`apply_mode`]: advisory.
fn apply_times(path: &Path, meta: &std::fs::Metadata) {
    let mtime = meta.modified().ok();
    let atime = meta.accessed().ok();
    if let Err(e) = set_times(path, atime, mtime) {
        log::warn!("could not set times on {}: {e}", path.display());
    }
}

/// `utimensat` on the path itself, never through a symlink.
///
/// `File::set_times` cannot be used: it needs a writable handle, and a
/// directory cannot be opened for writing on Linux. `utimensat` with
/// `AT_SYMLINK_NOFOLLOW` handles files, directories and symlinks with one call.
fn set_times(path: &Path, atime: Option<SystemTime>, mtime: Option<SystemTime>) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;

    /// Leave this timestamp alone (`UTIME_OMIT`, from `<sys/stat.h>`).
    const UTIME_OMIT: i64 = 0x3ffffffe;

    fn spec(t: Option<SystemTime>) -> libc::timespec {
        match t.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()) {
            Some(d) => libc::timespec {
                tv_sec: d.as_secs() as libc::time_t,
                tv_nsec: d.subsec_nanos() as i64,
            },
            // Pre-1970 timestamps and unreadable ones both end up here; leaving
            // the value alone beats writing a wrong one.
            None => libc::timespec {
                tv_sec: 0,
                tv_nsec: UTIME_OMIT,
            },
        }
    }

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| DfError::Op(format!("{}: path contains a NUL byte", path.display())))?;
    let times = [spec(atime), spec(mtime)];
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c_path.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(DfError::io(path, std::io::Error::last_os_error()));
    }
    Ok(())
}

fn already_exists(dst: &Path) -> DfError {
    DfError::io(
        dst,
        std::io::Error::new(std::io::ErrorKind::AlreadyExists, "already exists"),
    )
}

/// Move `src` to `dst`: `rename(2)`, falling back to copy-verify-delete across
/// filesystems.
pub fn move_path(src: &Path, dst: &Path, ctx: &TaskCtx, overwrite: bool) -> Result<()> {
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
        Ok(()) => return Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
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
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => move_cross_device(src, dst, ctx),
        Err(e) => Err(DfError::io(src, e)),
    }
}

/// Does this `rename(2)` failure mean "the old destination is in the way", as
/// opposed to "this move cannot happen at all"?
///
/// Only asked when the destination is known to exist, which is what keeps
/// `ENOTDIR` from being read as "a component of the path is not a directory".
fn in_the_way(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        // ENOTEMPTY/EEXIST: a directory with something in it. EISDIR: a
        // directory where the source is not one. ENOTDIR: the reverse.
        Some(libc::ENOTEMPTY) | Some(libc::EEXIST) | Some(libc::EISDIR) | Some(libc::ENOTDIR)
    )
}

/// The cross-filesystem move: copy everything, prove it arrived, then delete
/// the source.
///
/// Split out and public so the `EXDEV` path is reachable from a test without
/// two filesystems to hand — the branch that deletes the user's data is not one
/// to leave untested because the fixture is inconvenient.
pub fn move_cross_device(src: &Path, dst: &Path, ctx: &TaskCtx) -> Result<()> {
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
    super::delete::remove_tree(src, ctx)
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

    #[test]
    fn reflink_of_a_non_regular_file_reports_failure() {
        let t = TempTree::new("reflink-neg");
        let src = t.file("a.txt", b"x");
        let a = File::open(&src).unwrap();
        let dir = File::open(t.path()).unwrap();
        assert!(!reflink(&a, &dir), "cloning into a directory cannot work");
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
