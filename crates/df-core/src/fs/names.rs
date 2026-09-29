//! Names that have to be made up: `name_1`, `name_2`… when a paste, an
//! upload or a trash lands on a name that is taken, each clipped to fit a
//! 255-byte component.
//!
//! The yazi collision suffix, applied before the extension. It lived with the
//! trash, which was its first user; a paste, a download and an upload to a
//! server use it the same way, and none of them is the trash.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// How many `name_1`, `name_2`… variants to try before giving up on a name
/// collision inside the trash.
///
/// 1000 is far past any real case (it means a thousand files of the same name
/// deleted without ever emptying the trash) and stops a filesystem that lies
/// about `EEXIST` from spinning forever.
pub const MAX_TRASH_COLLISIONS: u32 = 1000;

/// The longest a single path component may be on ext4/btrfs/xfs. Names are
/// shortened to leave room for a `_12` suffix rather than failing with
/// `ENAMETOOLONG` on a name the user cannot see the length of.
pub const MAX_NAME_BYTES: usize = 255;

/// `name_1`, `name_2`… — the yazi collision suffix, applied before the
/// extension and clipped to fit a 255-byte name.
pub fn suffixed(name: &OsStr, n: u32) -> OsString {
    fit(name, Some(n), MAX_NAME_BYTES)
}

/// Build a name that fits in `max` bytes: `stem` + optional `_n` + `.ext`,
/// with the *stem* giving up bytes if something has to.
///
/// The extension is kept whole because it is what decides the icon, the opener
/// and the preview; a truncated stem is merely ugly, a truncated extension
/// changes what the file is. If the extension alone does not fit, the whole
/// name is clipped and the extension goes with it — nothing else is possible.
pub(crate) fn fit(name: &OsStr, suffix: Option<u32>, max: usize) -> OsString {
    let as_path = Path::new(name);
    let stem = bytes_of(as_path.file_stem().unwrap_or(name));
    let ext = as_path.extension().map(bytes_of);

    let suffix = suffix
        .map(|n| format!("_{n}").into_bytes())
        .unwrap_or_default();
    let ext_len = ext.as_ref().map(|e| e.len() + 1).unwrap_or(0);
    if suffix.len() + ext_len > max {
        // Pathological: an extension longer than a whole name may be.
        return name_of(clip(&bytes_of(name), max));
    }
    let room = max - suffix.len() - ext_len;

    let mut out = clip(&stem, room);
    out.extend_from_slice(&suffix);
    if let Some(ext) = ext {
        out.push(b'.');
        out.extend_from_slice(&ext);
    }
    name_of(out)
}

/// The bytes of a name: exactly the `OsStr` on Unix. A name the platform
/// cannot spell as bytes (not Unicode, on Windows) is spelled lossily — the
/// result is a *new* name to create, and a replacement character in it names
/// a file that can exist, where an error has nowhere to go.
fn bytes_of(name: &OsStr) -> Vec<u8> {
    crate::platform::os::as_bytes(name)
        .map(Cow::into_owned)
        .unwrap_or_else(|_| name.to_string_lossy().into_owned().into_bytes())
}

/// The name made of `bytes`. [`clip`] never splits a character, so bytes that
/// came from a Unicode name stay one; the lossy arm is never reached on Unix.
fn name_of(bytes: Vec<u8>) -> OsString {
    crate::platform::os::from_bytes(&bytes)
        .unwrap_or_else(|_| OsString::from(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Cut a byte string to at most `room` bytes without splitting a UTF-8
/// character — a half-written `é` in a filename is legal and horrible.
fn clip(bytes: &[u8], room: usize) -> Vec<u8> {
    if bytes.len() <= room {
        return bytes.to_vec();
    }
    let mut end = room;
    while end > 0 && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
        end -= 1;
    }
    bytes[..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixing_keeps_the_extension_and_the_length_limit() {
        assert_eq!(
            suffixed(OsStr::new("notes.txt"), 1),
            OsString::from("notes_1.txt")
        );
        assert_eq!(suffixed(OsStr::new("notes"), 2), OsString::from("notes_2"));
        assert_eq!(
            suffixed(OsStr::new("a.tar.gz"), 3),
            OsString::from("a.tar_3.gz")
        );
        assert_eq!(
            suffixed(OsStr::new(".bashrc"), 1),
            OsString::from(".bashrc_1"),
            "a dotfile has no extension to preserve"
        );

        let long = "x".repeat(MAX_NAME_BYTES);
        let out = suffixed(OsStr::new(&long), 12);
        assert!(out.len() <= MAX_NAME_BYTES, "{}", out.len());
        assert!(out.to_string_lossy().ends_with("_12"));
    }

    #[test]
    fn suffixing_never_splits_a_character() {
        let long = "é".repeat(200); // 400 bytes
        let out = suffixed(OsStr::new(&long), 1);
        assert!(out.len() <= MAX_NAME_BYTES);
        assert!(
            out.to_str().is_some(),
            "the clipped name is still valid UTF-8"
        );
    }
}
