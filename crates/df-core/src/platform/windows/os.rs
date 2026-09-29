//! A path's bytes, and a path from bytes, on Windows: the UTF-8 of the UTF-16
//! name. Every name that is valid Unicode converts exactly; one with an
//! unpaired surrogate — legal to NTFS, essentially never made by anything — has
//! no UTF-8, and is refused with [`DfError::Unsupported`] by whatever would
//! have written it down, never mangled (`00-ground-rules.md` §4).

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};

use crate::{DfError, Result};

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
