//! Extended attributes, the raw calls: what the file tags ([`crate::fs::tags`])
//! and a copy's `user.*` carrying are made of.
//!
//! Every call here is the `l` form (`lgetxattr`, `lsetxattr`, `llistxattr`,
//! `lremovexattr`), so a symlink is never followed.
//!
//! # The unsafe here
//!
//! `std` has no binding for extended attributes, and PLAN §1's bar for a new
//! dependency is "rewriting is impractical" — four syscalls with a
//! path, a name and a buffer each do not clear it. So the calls are made
//! through `libc` in the small functions under "The syscalls" below, each an
//! `unsafe` block with its own `#[allow(unsafe_code)]` (the workspace warns on
//! `unsafe_code` so every use is a deliberate one, as in the inotify watcher).
//! Nothing unsafe escapes them: the paths and names are `CString`s that
//! outlive the call, every buffer is a `Vec` whose length is the size handed
//! in, and every return value is checked and turned into
//! [`std::io::Error::last_os_error`].

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Whether this platform keeps extended attributes a tag can live in: yes.
pub const AVAILABLE: bool = true;

/// The attribute errors that mean "there is nothing here, and there never
/// will be": no such attribute, or a filesystem that keeps none. Neither is
/// worth more than a debug line.
pub fn quiet(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::ENODATA) | Some(libc::ENOTSUP) | Some(libc::ENOSYS)
    )
}

/// `ENODATA`: the file has no such attribute.
pub fn is_absent(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENODATA)
}

/// `ENOTSUP`: the filesystem keeps no `user.*` attributes (a FAT card, most
/// network mounts).
pub fn is_unsupported(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENOTSUP)
}

/// `EPERM`: refused — what Linux answers for a `user.*` attribute on a link.
pub fn is_not_permitted(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::EPERM)
}

// ── The syscalls ────────────────────────────────────────────────────────────

/// A path as the C calls take it.
fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

fn c_name(name: &str) -> io::Result<CString> {
    CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains a NUL byte"))
}

/// How big a first buffer is. A tag list is a few dozen bytes and an attribute
/// name list is the same, so one call answers almost every question; a value
/// longer than this is asked for again at its own size.
const FIRST_BUFFER: usize = 256;

/// Ask a sizing call twice: once into a buffer that is usually big enough,
/// and — on `ERANGE`, the value having grown since or being bigger than
/// guessed — again at the size the kernel reports. `call(buffer)` is the
/// syscall writing into `buffer`; an empty one asks for the size alone.
fn sized(mut call: impl FnMut(&mut [u8]) -> isize) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; FIRST_BUFFER];
    for _ in 0..4 {
        let n = call(&mut buffer);
        if n >= 0 {
            buffer.truncate(n as usize);
            return Ok(buffer);
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ERANGE) {
            return Err(e);
        }
        let needed = call(&mut []);
        if needed < 0 {
            return Err(io::Error::last_os_error());
        }
        buffer = vec![0u8; needed as usize + FIRST_BUFFER];
    }
    Err(io::Error::from_raw_os_error(libc::ERANGE))
}

/// One attribute's value, or `None` when the file has no such attribute.
pub fn get_raw(path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    let c_path = c_path(path)?;
    let c_name = c_name(name)?;
    match sized(|buffer| sys_get(&c_path, &c_name, buffer)) {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.raw_os_error() == Some(libc::ENODATA) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The names of every attribute on `path`, of every namespace.
pub fn list_raw(path: &Path) -> io::Result<Vec<String>> {
    let c_path = c_path(path)?;
    let bytes = sized(|buffer| sys_list(&c_path, buffer))?;
    // The kernel's list is names back to back, each ending in a NUL.
    Ok(bytes
        .split(|b| *b == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect())
}

/// Set one attribute, creating or replacing it.
pub fn set_raw(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    #[cfg(test)]
    if REFUSING.with(std::cell::Cell::get) {
        return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    let c_path = c_path(path)?;
    let c_name = c_name(name)?;
    if sys_set(&c_path, &c_name, value) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Remove one attribute. `ENODATA` when there was none, left to the caller.
pub fn remove_raw(path: &Path, name: &str) -> io::Result<()> {
    let c_path = c_path(path)?;
    let c_name = c_name(name)?;
    if sys_remove(&c_path, &c_name) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// The four calls themselves, one `unsafe` block each and nothing else, so the
// whole of what is being trusted is in view at once.

/// `lgetxattr(2)` into `buffer`; an empty one asks for the size alone.
#[allow(unsafe_code)]
fn sys_get(path: &CString, name: &CString, buffer: &mut [u8]) -> isize {
    // SAFETY: both strings are NUL-terminated and borrowed for the length of
    // the call, and the kernel writes at most `buffer.len()` bytes into the
    // buffer — none at all when that is zero.
    unsafe {
        libc::lgetxattr(
            path.as_ptr(),
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    }
}

/// `llistxattr(2)`, into `buffer` as [`sys_get`] takes it.
#[allow(unsafe_code)]
fn sys_list(path: &CString, buffer: &mut [u8]) -> isize {
    // SAFETY: as `sys_get`: a borrowed NUL-terminated path, and a buffer the
    // kernel writes at most its own length into.
    unsafe { libc::llistxattr(path.as_ptr(), buffer.as_mut_ptr().cast(), buffer.len()) }
}

/// `lsetxattr(2)`, flags 0: create or replace.
#[allow(unsafe_code)]
fn sys_set(path: &CString, name: &CString, value: &[u8]) -> libc::c_int {
    // SAFETY: two borrowed NUL-terminated strings, and `value` is a live
    // slice the kernel reads exactly `value.len()` bytes of.
    unsafe {
        libc::lsetxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    }
}

/// `lremovexattr(2)`.
#[allow(unsafe_code)]
fn sys_remove(path: &CString, name: &CString) -> libc::c_int {
    // SAFETY: two borrowed NUL-terminated strings; nothing is written.
    unsafe { libc::lremovexattr(path.as_ptr(), name.as_ptr()) }
}

#[cfg(test)]
thread_local! {
    /// Stand in for a destination that keeps no attributes (a FAT card), on
    /// this thread: every set answers `ENOTSUP`. There is no such filesystem
    /// a test can make without mounting one.
    static REFUSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with every attribute write on this thread refused as a FAT card
/// refuses it.
#[cfg(test)]
pub(crate) fn refusing<T>(f: impl FnOnce() -> T) -> T {
    REFUSING.with(|c| c.set(true));
    let out = f();
    REFUSING.with(|c| c.set(false));
    out
}

/// Whether this machine's `$TMPDIR` holds `user.*` attributes, for the tests
/// that need one — tmpfs does from Linux 6.6, ext4 and btrfs always have.
/// Says why when it does not, so a skipped test is a visible one.
#[cfg(test)]
pub(crate) fn supported_here(probe: &Path) -> bool {
    match set_raw(probe, "user.df-probe", b"1") {
        Ok(()) => {
            let _ = remove_raw(probe, "user.df-probe");
            true
        }
        Err(e) => {
            eprintln!(
                "skipping: {} holds no user.* attributes ({e})",
                probe.display()
            );
            false
        }
    }
}
