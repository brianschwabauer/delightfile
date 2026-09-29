//! Windows file primitives: the ones `std` can do in a call or two, a file's
//! identity from its handle ([`same_file`], through
//! [`crate::platform::meta::identity`]), and honest answers for the rest until
//! W4.6.

use std::fs::File;
use std::path::Path;
use std::time::SystemTime;

use crate::{DfError, Result};

pub use crate::platform::stub::fs::*;

/// Make a symbolic link at `link` whose text is `target`. Windows has two
/// kinds, and which one is decided by what the target is — resolved from the
/// link's own folder, as it will be when followed. A target that does not
/// exist gets a file link. Needs Developer Mode or elevation (W4.6 words the
/// refusal).
pub fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    let resolved = match link.parent() {
        Some(dir) => dir.join(target),
        None => target.to_path_buf(),
    };
    if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// Windows keeps one permission bit: read-only. It is set when `mode` grants
/// nobody write, and cleared otherwise.
pub fn apply_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o222 == 0);
    std::fs::set_permissions(path, permissions)
}

/// Set the access and modification times on `path` itself, never through a
/// symlink: a handle opened for attribute writes on the reparse point, with
/// backup semantics so a directory opens too. A missing time is left alone.
pub fn set_times(path: &Path, atime: Option<SystemTime>, mtime: Option<SystemTime>) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| DfError::io(path, e))?;
    let mut times = std::fs::FileTimes::new();
    if let Some(atime) = atime {
        times = times.set_accessed(atime);
    }
    if let Some(mtime) = mtime {
        times = times.set_modified(mtime);
    }
    file.set_times(times).map_err(|e| DfError::io(path, e))
}

/// Nothing to do: a directory on Windows has no `fsync`, and a renamed name is
/// durable once the file system has journalled the rename.
pub fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Write all of `data` at `offset`: `seek_write` until it is all out, since a
/// single call may write less.
pub fn write_all_at(file: &File, mut data: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        match file.seek_write(data, offset) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
            Ok(n) => {
                data = &data[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Are these two paths the same file *on disk* — the same volume serial and
/// file index, as `GetFileInformationByHandle` reports them — neither followed
/// if it is a link? The answer that holds however the two are spelled: in
/// another case, through a junction, as a short `8.3` name. An error when
/// either cannot be opened.
pub fn same_file(a: &Path, b: &Path) -> std::io::Result<bool> {
    let of = |path: &Path| {
        let meta = std::fs::symlink_metadata(path)?;
        crate::platform::meta::identity(path, &meta)
    };
    let (a, b) = (of(a)?, of(b)?);
    Ok(a.dev == b.dev && a.ino == b.ino)
}

/// `IO_REPARSE_TAG_MOUNT_POINT`, from `winnt.h`: the tag of a junction, and
/// of a volume mounted in a folder.
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// Whether `path` itself is a junction (or a volume mounted in a folder):
/// a link std reports as a directory symlink, whose target is always an
/// absolute path on this machine — not something an archive can carry.
pub fn is_junction(path: &Path) -> bool {
    crate::platform::meta::reparse_tag(path) == Some(IO_REPARSE_TAG_MOUNT_POINT)
}

/// Remove the link at `path` itself, never what it points at. Windows has
/// two kinds: a link to a directory, and a junction, are directories to the
/// file system and go with `RemoveDirectoryW`; a link to a file goes with
/// `DeleteFileW`.
pub fn remove_link(path: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::FileTypeExt;
    if std::fs::symlink_metadata(path)?
        .file_type()
        .is_symlink_dir()
    {
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Assumed: the CRT's `_access(path, 2)` ignores the read-only bit on a
/// directory and would say the same. W4.6 replaces this with a real probe.
pub fn writable(_dir: &Path) -> bool {
    true
}
