//! A path's bytes, and a path from bytes, on Windows: the UTF-8 of the UTF-16
//! name. Every name that is valid Unicode converts exactly; one with an
//! unpaired surrogate — legal to NTFS, essentially never made by anything — has
//! no UTF-8, and is refused with [`DfError::Unsupported`] by whatever would
//! have written it down, never mangled (`00-ground-rules.md` §4).
//!
//! Beside the conversion, what Windows holds a name to, as the constants
//! [`crate::path`] reads: its reserved characters and device names, and one
//! file for every spelling that differs only in case.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};

use crate::{DfError, Result};

/// Whether names are held to Windows' rules (reserved characters and device
/// names, no trailing dot or space): yes — the system refuses them, or makes
/// a file nothing else can open.
pub const STRICT_NAMES: bool = true;

/// Whether a table keyed by path folds case ([`crate::path::key`]): yes, as
/// NTFS does, so `C:\Users\x` and `c:\users\X` find one record.
pub const FOLD_CASE: bool = true;

/// The longest one name may be, in the unit [`NAME_IN_UTF16`] names: 255
/// UTF-16 code units, NTFS's limit.
pub const MAX_NAME: usize = 255;

/// Whether [`MAX_NAME`] counts UTF-16 code units: yes — a name is UTF-16
/// here, so `é` costs one and `🎬` two, where UTF-8 would say two and four.
pub const NAME_IN_UTF16: bool = true;

/// Whether a place under home is shown from `~`: no — Explorer, the address
/// bar and every dialog write `C:\Users\admin\Downloads`, and `~` is a Unix
/// shell's (04-windows.md W4.40).
pub const HOME_AS_TILDE: bool = false;

/// The UTF-8 of `s`, or a refusal when it is not valid Unicode.
pub fn as_bytes(s: &OsStr) -> Result<Cow<'_, [u8]>> {
    s.to_str()
        .map(|text| Cow::Borrowed(text.as_bytes()))
        .ok_or(DfError::Unsupported("non-Unicode file name"))
}

/// The name whose UTF-8 is `b`, or a refusal when `b` is not UTF-8.
pub fn from_bytes(b: &[u8]) -> Result<OsString> {
    std::str::from_utf8(b)
        .map(OsString::from)
        .map_err(|_| DfError::Unsupported("non-Unicode file name"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use std::os::windows::ffi::OsStringExt;

    #[test]
    fn unicode_names_convert_and_a_lone_surrogate_is_refused() {
        let name = from_bytes("café.txt".as_bytes()).unwrap();
        assert_eq!(as_bytes(&name).unwrap().as_ref(), "café.txt".as_bytes());
        assert!(from_bytes(b"caf\xE9.txt").is_err());
        let lone = OsString::from_wide(&[0x61, 0xD800, 0x62]);
        assert!(as_bytes(&lone).is_err());
    }
}
