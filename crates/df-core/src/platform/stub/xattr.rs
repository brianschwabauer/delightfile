//! No extended attributes: stands in on macOS and on Windows.
//!
//! Reads answer "no attributes" — no value, an empty list — so no row has
//! tags, a tag search finds nothing, and a copy carries nothing and reports
//! nothing lost. Writes refuse, and [`AVAILABLE`] is `false` so the tags
//! prompt refuses with "Tags is not available on this platform" before it
//! gets here. macOS does have attributes; whether its tags are freedesktop's
//! `user.xdg.tags` or Finder's `com.apple.metadata:_kMDItemUserTags` is an
//! open question (`plans/other-platforms/02-macos.md`), so it has no body yet.

use std::io;
use std::path::Path;

/// Whether this platform keeps extended attributes a tag can live in: not yet.
pub const AVAILABLE: bool = false;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        crate::DfError::Unsupported("Tags"),
    )
}

/// The refusal a write here gives, and nothing a read here gives, is "nothing
/// to see".
pub fn quiet(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Unsupported
}

/// Never: nothing here reads an attribute to find it missing.
pub fn is_absent(_e: &io::Error) -> bool {
    false
}

/// Never in the sense of a filesystem that keeps none: the refusal here is the
/// platform's, and is worded as such.
pub fn is_unsupported(_e: &io::Error) -> bool {
    false
}

/// Never.
pub fn is_not_permitted(_e: &io::Error) -> bool {
    false
}

/// No attribute.
pub fn get_raw(_path: &Path, _name: &str) -> io::Result<Option<Vec<u8>>> {
    Ok(None)
}

/// No attributes.
pub fn list_raw(_path: &Path) -> io::Result<Vec<String>> {
    Ok(Vec::new())
}

/// Refused.
pub fn set_raw(_path: &Path, _name: &str, _value: &[u8]) -> io::Result<()> {
    Err(unsupported())
}

/// Refused.
pub fn remove_raw(_path: &Path, _name: &str) -> io::Result<()> {
    Err(unsupported())
}

/// Every write here is refused already.
#[cfg(test)]
pub(crate) fn refusing<T>(f: impl FnOnce() -> T) -> T {
    f()
}

/// No: the tests that need attributes skip, and say so.
#[cfg(test)]
pub(crate) fn supported_here(probe: &Path) -> bool {
    eprintln!(
        "skipping: {} can hold no attributes on this platform",
        probe.display()
    );
    false
}
