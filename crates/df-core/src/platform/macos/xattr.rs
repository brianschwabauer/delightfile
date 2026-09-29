//! Extended attributes on macOS, and the file tags as Finder keeps them.
//!
//! macOS has extended attributes as Linux has, through `getxattr`,
//! `setxattr`, `listxattr` and `removexattr` with `XATTR_NOFOLLOW` (so a
//! link is never followed), and any name is allowed: a `user.*` attribute a
//! copy carries ([`crate::fs::tags::carry`]) is set and read as it is on
//! Linux.
//!
//! The tags are the exception. On a Mac they are **Finder's tags**, so a tag
//! set here shows in Finder and Spotlight and the other way round: the
//! attribute is `com.apple.metadata:_kMDItemUserTags`, a binary plist of the
//! tag names ([`super::bplist`]), each with an optional `\n<colour index>`
//! suffix. `fs::tags` asks for its own attribute
//! ([`crate::fs::tags::XATTR`], `user.xdg.tags`, a comma list), and this
//! module answers in its words from Finder's:
//!
//! - **Reading** gives the names, suffixes taken off, joined with commas.
//! - **Writing** a comma list gives each name back its colour: the one Finder
//!   already had for it on this file, else Finder's colour for the seven
//!   colour tags (`red` is 6, `grey` is 1…), else none. So a copy — which
//!   reads the source's tags and writes them onto the destination — keeps
//!   Finder's colours for the colour tags, and for any tag when the
//!   destination already carries Finder's attribute (a clone does); a custom
//!   tag Finder coloured is otherwise written uncoloured.
//! - **Listing** names Finder's attribute as `user.xdg.tags`, so the walk
//!   that looks for tagged files, and the copy that carries every `user.*`
//!   attribute, find it; a literal `user.xdg.tags` (from a disk tagged on
//!   Linux) is hidden behind it, and Linux's tags are not read on a Mac.
//! - A link can hold attributes on macOS, but a tag on a link is refused as
//!   on Linux (`EPERM`, which `fs::tags` words as "Links can't hold tags"),
//!   so tagging a link means the same thing everywhere.
//!
//! A tag name holding a comma — which Finder allows and delightfile refuses to
//! write — reads as two tags.
//!
//! # The unsafe here
//!
//! The four calls are one `unsafe` block each, in the functions under "The
//! syscalls", with the rules of the Linux body: paths and names are
//! `CString`s that outlive the call, every buffer is a `Vec` whose length is
//! the size handed in, and every return value is checked and turned into
//! [`io::Error::last_os_error`].

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::bplist;
use crate::fs::tags::XATTR as TAGS;

/// Whether this platform keeps extended attributes a tag can live in: yes.
pub const AVAILABLE: bool = true;

/// Where Finder keeps a file's tags.
const FINDER_TAGS: &str = "com.apple.metadata:_kMDItemUserTags";

/// Finder's colour index for each of the colour tags `fs::tags` offers
/// ([`crate::fs::tags::COLOURS`]). Index 0 is "no colour" and is not
/// written.
const FINDER_COLOURS: [(&str, u8); 7] = [
    ("grey", 1),
    ("green", 2),
    ("purple", 3),
    ("blue", 4),
    ("yellow", 5),
    ("red", 6),
    ("orange", 7),
];

/// The attribute errors that mean "there is nothing here, and there never
/// will be": no such attribute, or a filesystem that keeps none.
pub fn quiet(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::ENOATTR) | Some(libc::ENOTSUP) | Some(libc::ENOSYS)
    )
}

/// `ENOATTR`: the file has no such attribute.
pub fn is_absent(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENOATTR)
}

/// `ENOTSUP`: the filesystem keeps no extended attributes.
pub fn is_unsupported(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENOTSUP)
}

/// `EPERM`: refused — what a tag on a link is answered with here.
pub fn is_not_permitted(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::EPERM)
}

// ── The tags, in Finder's words ─────────────────────────────────────────────

/// A Finder tag's name, its `\n<index>` colour taken off.
fn tag_name(tag: &str) -> &str {
    match tag.rsplit_once('\n') {
        Some((name, index)) if index.parse::<u8>().is_ok() => name,
        _ => tag,
    }
}

/// A Finder tag's colour, `0` for none.
fn tag_colour(tag: &str) -> u8 {
    tag.rsplit_once('\n')
        .and_then(|(_, index)| index.parse::<u8>().ok())
        .unwrap_or(0)
}

/// Finder's tags as the comma list `fs::tags` reads.
fn tags_from_finder(value: &[u8]) -> io::Result<Vec<u8>> {
    let tags = bplist::decode_strings(value).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Finder's tags are not a list of names",
        )
    })?;
    let names: Vec<&str> = tags.iter().map(|tag| tag_name(tag)).collect();
    Ok(names.join(",").into_bytes())
}

/// A comma list as Finder's tags, each name given the colour `existing`
/// gives it (Finder's tags on the file now), else a colour tag's own.
fn tags_for_finder(value: &[u8], existing: &[String]) -> Vec<u8> {
    let text = String::from_utf8_lossy(value);
    let tags: Vec<String> = text
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            let kept = existing
                .iter()
                .find(|tag| crate::fs::tags::same(tag_name(tag), name))
                .map(|tag| tag_colour(tag))
                .filter(|colour| *colour != 0);
            let own = FINDER_COLOURS
                .iter()
                .find(|(colour, _)| crate::fs::tags::same(colour, name))
                .map(|(_, index)| *index);
            match kept.or(own) {
                Some(index) => format!("{name}\n{index}"),
                None => name.to_string(),
            }
        })
        .collect();
    bplist::encode_strings(&tags)
}

// ── The calls `fs::tags` makes ──────────────────────────────────────────────

/// One attribute's value, or `None` when the file has none by that name. The
/// tags are Finder's, as a comma list.
pub fn get_raw(path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    if name == TAGS {
        return match get(path, FINDER_TAGS)? {
            Some(value) => tags_from_finder(&value).map(Some),
            None => Ok(None),
        };
    }
    get(path, name)
}

/// The names of every attribute on `path`, Finder's tags among them as
/// [`TAGS`].
pub fn list_raw(path: &Path) -> io::Result<Vec<String>> {
    let names = list(path)?;
    Ok(names
        .into_iter()
        .filter(|name| name != TAGS)
        .map(|name| {
            if name == FINDER_TAGS {
                TAGS.to_string()
            } else {
                name
            }
        })
        .collect())
}

/// Set one attribute, creating or replacing it. The tags go to Finder's,
/// coloured as the module note says. Refused on a link.
pub fn set_raw(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    #[cfg(test)]
    if REFUSING.with(std::cell::Cell::get) {
        return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::from_raw_os_error(libc::EPERM));
    }
    if name == TAGS {
        let existing = get(path, FINDER_TAGS)?
            .and_then(|value| bplist::decode_strings(&value))
            .unwrap_or_default();
        return set(path, FINDER_TAGS, &tags_for_finder(value, &existing));
    }
    set(path, name, value)
}

/// Remove one attribute. `ENOATTR` when there was none, left to the caller.
pub fn remove_raw(path: &Path, name: &str) -> io::Result<()> {
    remove(path, if name == TAGS { FINDER_TAGS } else { name })
}

// ── The syscalls ────────────────────────────────────────────────────────────

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

fn c_name(name: &str) -> io::Result<CString> {
    CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains a NUL byte"))
}

/// How big a first buffer is: a tag list and a name list are a few dozen
/// bytes, so one call answers almost every question.
const FIRST_BUFFER: usize = 256;

/// Ask a sizing call twice, as the Linux body does: into a buffer that is
/// usually big enough, and on `ERANGE` again at the size the kernel reports.
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

fn get(path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    let c_path = c_path(path)?;
    let c_name = c_name(name)?;
    match sized(|buffer| sys_get(&c_path, &c_name, buffer)) {
        Ok(value) => Ok(Some(value)),
        Err(e) if is_absent(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

fn list(path: &Path) -> io::Result<Vec<String>> {
    let c_path = c_path(path)?;
    let bytes = sized(|buffer| sys_list(&c_path, buffer))?;
    Ok(bytes
        .split(|b| *b == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect())
}

fn set(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    let c_path = c_path(path)?;
    let c_name = c_name(name)?;
    if sys_set(&c_path, &c_name, value) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn remove(path: &Path, name: &str) -> io::Result<()> {
    let c_path = c_path(path)?;
    let c_name = c_name(name)?;
    if sys_remove(&c_path, &c_name) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `getxattr(2)` into `buffer`, not following a link; an empty buffer asks
/// for the size alone.
#[allow(unsafe_code)]
fn sys_get(path: &CString, name: &CString, buffer: &mut [u8]) -> isize {
    let (ptr, len) = if buffer.is_empty() {
        (std::ptr::null_mut(), 0)
    } else {
        (buffer.as_mut_ptr().cast(), buffer.len())
    };
    // SAFETY: both strings are NUL-terminated and borrowed for the call, and
    // the kernel writes at most `len` bytes through `ptr` — none when it is
    // null, which asks for the size.
    unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            ptr,
            len,
            0,
            libc::XATTR_NOFOLLOW,
        )
    }
}

/// `listxattr(2)`, into `buffer` as [`sys_get`] takes it.
#[allow(unsafe_code)]
fn sys_list(path: &CString, buffer: &mut [u8]) -> isize {
    let (ptr, len) = if buffer.is_empty() {
        (std::ptr::null_mut(), 0)
    } else {
        (buffer.as_mut_ptr().cast(), buffer.len())
    };
    // SAFETY: as `sys_get`.
    unsafe { libc::listxattr(path.as_ptr(), ptr, len, libc::XATTR_NOFOLLOW) }
}

/// `setxattr(2)`, create or replace, not following a link.
#[allow(unsafe_code)]
fn sys_set(path: &CString, name: &CString, value: &[u8]) -> libc::c_int {
    // SAFETY: two borrowed NUL-terminated strings, and `value` is a live
    // slice the kernel reads exactly `value.len()` bytes of.
    unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    }
}

/// `removexattr(2)`, not following a link.
#[allow(unsafe_code)]
fn sys_remove(path: &CString, name: &CString) -> libc::c_int {
    // SAFETY: two borrowed NUL-terminated strings; nothing is written.
    unsafe { libc::removexattr(path.as_ptr(), name.as_ptr(), libc::XATTR_NOFOLLOW) }
}

#[cfg(test)]
thread_local! {
    /// Stand in for a destination that keeps no attributes, on this thread.
    static REFUSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with every attribute write on this thread refused as a disk that
/// keeps none refuses it.
#[cfg(test)]
pub(crate) fn refusing<T>(f: impl FnOnce() -> T) -> T {
    REFUSING.with(|c| c.set(true));
    let out = f();
    REFUSING.with(|c| c.set(false));
    out
}

/// Whether this machine's `$TMPDIR` holds extended attributes, for the tests
/// that need one. Says why when it does not.
#[cfg(test)]
pub(crate) fn supported_here(probe: &Path) -> bool {
    match set_raw(probe, "user.df-probe", b"1") {
        Ok(()) => {
            let _ = remove_raw(probe, "user.df-probe");
            true
        }
        Err(e) => {
            eprintln!(
                "skipping: {} holds no extended attributes ({e})",
                probe.display()
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::fs::tags;
    use crate::ops::fixture::TempTree;

    fn list_of(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// Tags written here are Finder's: its attribute, its plist, its colour
    /// indices on the colour tags — so Finder shows them.
    #[test]
    fn tags_are_written_as_finder_writes_them() {
        let t = TempTree::new("finder-tags");
        let file = t.file("a.txt", b"x");
        tags::write(&file, &list_of(&["Red", "work", "grey"])).unwrap();
        let raw = get(&file, FINDER_TAGS).unwrap().unwrap();
        assert!(raw.starts_with(b"bplist00"));
        assert_eq!(
            bplist::decode_strings(&raw),
            Some(list_of(&["Red\n6", "work", "grey\n1"]))
        );
        assert_eq!(
            get(&file, TAGS).unwrap(),
            None,
            "no attribute of Linux's name"
        );
        assert_eq!(tags::read(&file), ["Red", "work", "grey"]);
        let names = list_raw(&file).unwrap();
        assert!(names.iter().any(|name| name == TAGS), "{names:?}");
        assert!(!names.iter().any(|name| name == FINDER_TAGS), "{names:?}");
    }

    /// Tags Finder wrote are read, colours off; a colour Finder gave a tag of
    /// its own is kept when the tags are written again; and a copy carries
    /// them.
    #[test]
    fn finders_tags_are_read_and_their_colours_kept() {
        let t = TempTree::new("finder-tags-read");
        let file = t.file("a.txt", b"x");
        let finder = bplist::encode_strings(&list_of(&["Important\n6", "Blue\n4", "plain"]));
        set(&file, FINDER_TAGS, &finder).unwrap();
        assert_eq!(tags::read(&file), ["Important", "Blue", "plain"]);

        tags::write(&file, &list_of(&["Important", "plain", "new"])).unwrap();
        let raw = get(&file, FINDER_TAGS).unwrap().unwrap();
        assert_eq!(
            bplist::decode_strings(&raw),
            Some(list_of(&["Important\n6", "plain", "new"]))
        );

        set(&file, "user.comment", b"from the camera").unwrap();
        let copy = t.file("b.txt", b"x");
        assert!(!tags::carry(&file, &copy), "nothing was refused");
        assert_eq!(tags::read(&copy), ["Important", "plain", "new"]);
        assert_eq!(
            get(&copy, "user.comment").unwrap().as_deref(),
            Some(b"from the camera".as_slice())
        );
    }

    /// No tags is no attribute, and a link is refused rather than tagged.
    #[test]
    fn untagging_removes_finders_attribute_and_a_link_is_refused() {
        let t = TempTree::new("finder-tags-remove");
        let file = t.file("a.txt", b"x");
        tags::write(&file, &list_of(&["red"])).unwrap();
        tags::write(&file, &[]).unwrap();
        assert_eq!(get(&file, FINDER_TAGS).unwrap(), None);
        assert!(!list_raw(&file).unwrap().iter().any(|name| name == TAGS));

        let link = t.symlink(&file, "link");
        let err = tags::write(&link, &list_of(&["blue"])).unwrap_err();
        assert!(matches!(err, tags::TagError::Link), "{err}");
    }
}
