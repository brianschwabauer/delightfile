//! Names that have to be made up: `name_1`, `name_2`… when a paste, an
//! upload or a trash lands on a name that is taken, each clipped to fit a
//! component the file system will take.
//!
//! The yazi collision suffix, applied before the extension. It lived with the
//! trash, which was its first user; a paste, a download and an upload to a
//! server use it the same way, and none of them is the trash.
//!
//! How long a name may be is the platform's to say
//! ([`crate::platform::os::MAX_NAME`]), and so is the unit it counts in: 255
//! bytes on Unix, 255 UTF-16 code units on Windows, where `é` costs one unit
//! rather than two bytes and `🎬` two rather than four. Either way a name is
//! cut between characters, never inside one.

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
/// `ENAMETOOLONG` on a name the user cannot see the length of. The
/// freedesktop trash, which is Linux's, budgets its `.trashinfo` names in it;
/// what a suffixed name may take on this platform is
/// [`crate::platform::os::MAX_NAME`].
pub const MAX_NAME_BYTES: usize = 255;

/// `name_1`, `name_2`… — the yazi collision suffix, applied before the
/// extension and clipped to fit a name this platform takes.
pub fn suffixed(name: &OsStr, n: u32) -> OsString {
    fit(name, Some(n), crate::platform::os::MAX_NAME)
}

/// Build a name that fits in `max` of this platform's name units: `stem` +
/// optional `_n` + `.ext`, with the *stem* giving up length if something has
/// to.
///
/// The extension is kept whole because it is what decides the icon, the opener
/// and the preview; a truncated stem is merely ugly, a truncated extension
/// changes what the file is. If the extension alone does not fit, the whole
/// name is clipped and the extension goes with it — nothing else is possible.
pub(crate) fn fit(name: &OsStr, suffix: Option<u32>, max: usize) -> OsString {
    fit_counted(name, suffix, max, crate::platform::os::NAME_IN_UTF16)
}

/// [`fit`], counting in UTF-16 code units when `utf16` says so and in bytes
/// otherwise — both rules, so both are tested on every target.
fn fit_counted(name: &OsStr, suffix: Option<u32>, max: usize, utf16: bool) -> OsString {
    let as_path = Path::new(name);
    let stem = bytes_of(as_path.file_stem().unwrap_or(name));
    let ext = as_path.extension().map(bytes_of);

    let suffix = suffix
        .map(|n| format!("_{n}").into_bytes())
        .unwrap_or_default();
    let ext_len = ext.as_ref().map(|e| units(e, utf16) + 1).unwrap_or(0);
    if suffix.len() + ext_len > max {
        // Pathological: an extension longer than a whole name may be.
        return name_of(clip(&bytes_of(name), max, utf16));
    }
    let room = max - suffix.len() - ext_len;

    let mut out = clip(&stem, room, utf16);
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

/// How long `bytes` is as a name: its bytes, or its UTF-16 code units. The
/// UTF-16 count is of the UTF-8 view, which is all a Windows name has.
fn units(bytes: &[u8], utf16: bool) -> usize {
    if utf16 {
        String::from_utf8_lossy(bytes).encode_utf16().count()
    } else {
        bytes.len()
    }
}

/// Cut a name to at most `room` units without splitting a character — a
/// half-written `é` in a filename is legal on Unix and horrible, and half a
/// surrogate pair is not a Windows name at all.
fn clip(bytes: &[u8], room: usize, utf16: bool) -> Vec<u8> {
    if !utf16 {
        return clip_bytes(bytes, room);
    }
    let text = String::from_utf8_lossy(bytes);
    let mut used = 0;
    let mut end = 0;
    for (at, c) in text.char_indices() {
        if used + c.len_utf16() > room {
            break;
        }
        used += c.len_utf16();
        end = at + c.len_utf8();
    }
    text.as_bytes()[..end].to_vec()
}

/// [`clip`] in bytes, cutting only at a UTF-8 character boundary.
fn clip_bytes(bytes: &[u8], room: usize) -> Vec<u8> {
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
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    /// A name's length as this platform counts it.
    fn length(name: &OsStr) -> usize {
        units(&bytes_of(name), crate::platform::os::NAME_IN_UTF16)
    }

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

        let long = "x".repeat(crate::platform::os::MAX_NAME);
        let out = suffixed(OsStr::new(&long), 12);
        assert!(
            length(&out) <= crate::platform::os::MAX_NAME,
            "{}",
            out.len()
        );
        assert!(out.to_string_lossy().ends_with("_12"));
    }

    #[test]
    fn suffixing_never_splits_a_character() {
        let long = "é".repeat(300); // 600 bytes, 300 UTF-16 units
        let out = suffixed(OsStr::new(&long), 1);
        assert!(length(&out) <= crate::platform::os::MAX_NAME);
        assert!(
            out.to_str().is_some(),
            "the clipped name is still valid UTF-8"
        );
    }

    /// A UTF-8 name keeps its extension and its characters in both units:
    /// what a byte budget and a UTF-16 budget each let through.
    #[test]
    fn a_unicode_name_is_clipped_by_bytes_or_by_utf16_units() {
        let name = format!("{}.jpg", "é".repeat(200));
        let bytes = fit_counted(OsStr::new(&name), Some(1), 255, false);
        let bytes = bytes.to_str().unwrap();
        assert!(bytes.len() <= 255, "{}", bytes.len());
        assert!(bytes.ends_with("_1.jpg"));
        assert_eq!(bytes.chars().filter(|c| *c == 'é').count(), 124);

        let units = fit_counted(OsStr::new(&name), Some(1), 255, true);
        let units = units.to_str().unwrap();
        assert_eq!(units, format!("{}_1.jpg", "é".repeat(200)), "fits whole");

        let long = format!("{}.jpg", "é".repeat(300));
        let clipped = fit_counted(OsStr::new(&long), Some(1), 255, true);
        let clipped = clipped.to_str().unwrap();
        assert_eq!(clipped.encode_utf16().count(), 255);
        assert!(clipped.ends_with("_1.jpg"));
    }

    /// An emoji is two UTF-16 units; a budget that has room for one of them
    /// leaves the whole character out rather than half of it.
    #[test]
    fn a_surrogate_pair_is_never_split() {
        let name = "🎬".repeat(10);
        let out = fit_counted(OsStr::new(&name), None, 5, true);
        assert_eq!(out.to_str(), Some("🎬🎬"));
        let out = fit_counted(OsStr::new(&name), None, 5, false);
        assert_eq!(out.to_str(), Some("🎬"), "four bytes each");
    }
}
