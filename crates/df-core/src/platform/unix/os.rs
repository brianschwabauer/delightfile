//! A path's bytes, and a path from bytes: the one door between `OsStr` and the
//! byte strings the state file, git's output, rsync's itemized lines and the
//! archive writer's member names are made of.
//!
//! On Unix a path *is* bytes, so both directions are exact and neither can
//! fail: a name that is not UTF-8 — off a FAT stick, out of an archive — goes
//! through untouched. The `Result` is for Windows, where a path is UTF-16 and
//! the byte form is its UTF-8, which a lone surrogate does not have.
//!
//! Beside the conversion, what this platform's names are held to, as the
//! constants [`crate::path`] reads: Unix refuses only NUL and `/` in a name,
//! and a lookup table keys a path as it is spelled (macOS too — its spellings
//! come from `read_dir` on both sides, `03-paths.md`).

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use crate::Result;

/// Whether names are held to Windows' rules (reserved characters and device
/// names, no trailing dot or space): not here.
pub const STRICT_NAMES: bool = false;

/// Whether a table keyed by path folds case ([`crate::path::key`]): not
/// here, where two spellings are two files, or come from one `read_dir`.
pub const FOLD_CASE: bool = false;

/// The longest one name may be, in the unit [`NAME_IN_UTF16`] names: 255
/// bytes, the limit of ext4, btrfs, xfs and APFS alike.
pub const MAX_NAME: usize = 255;

/// Whether [`MAX_NAME`] counts UTF-16 code units: no, bytes.
pub const NAME_IN_UTF16: bool = false;

/// Whether a place under home is shown from `~` (`~/Work`): yes, the way a
/// Unix shell and every Unix program writes it.
pub const HOME_AS_TILDE: bool = true;

/// The bytes of `s`, exactly as the kernel has them. Never an error here.
pub fn as_bytes(s: &OsStr) -> Result<Cow<'_, [u8]>> {
    Ok(Cow::Borrowed(s.as_bytes()))
}

/// The name `b` spells, byte for byte. Never an error here.
pub fn from_bytes(b: &[u8]) -> Result<OsString> {
    Ok(OsString::from_vec(b.to_vec()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    /// A name that is not UTF-8 goes both ways unchanged.
    #[test]
    fn bytes_round_trip_whatever_they_spell() {
        let raw = b"caf\xE9 \xff.txt";
        let name = from_bytes(raw).unwrap();
        assert_eq!(as_bytes(&name).unwrap().as_ref(), raw);
        assert_eq!(as_bytes(OsStr::new("plain")).unwrap().as_ref(), b"plain");
    }
}
