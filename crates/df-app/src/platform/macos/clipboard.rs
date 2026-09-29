//! The clipboard on macOS: `NSPasteboard`, asked directly.
//!
//! The pasteboard is synchronous, and that decides the shape of everything
//! here. AppKit copies the bytes into the pasteboard server on `setData:` (or
//! `writeObjects:`), so a copy is finished when [`copy`] returns: there is no
//! server process of ours to keep alive, and [`copy`] answers `Ok(None)` — a
//! copy already made, which the window toasts at once. A paste is a read that
//! returns the bytes.
//!
//! What goes on the pasteboard, and under which name, is the table in
//! [`crate::platform::pasteboard`]; this file is the AppKit half. A list of
//! files goes as one `NSURL` per file, which is what Finder writes and reads.
//! A list of files is read back item by item, each item's `public.file-url`
//! resolved through `NSURL`, so Finder's file *reference* URLs
//! (`file:///.file/id=…`) arrive as the paths they refer to; it is handed up
//! as a `text/uri-list` through [`crate::clipboard::uri_list`], so the
//! window's one parser reads it.
//!
//! **Unsafe.** Every method objc2-app-kit 0.2 generates is `unsafe`, because
//! the generator cannot know which Objective-C preconditions each has. The
//! ones called here have none beyond what the types already say: the
//! pasteboard, strings, URLs and data are objects this file made or AppKit
//! returned as `Retained` (so they are alive for the call), nothing is passed
//! as `nil` where AppKit does not allow it, and every call that hands back
//! autoreleased objects runs inside an [`autoreleasepool`], which drains them
//! whatever thread the call is made on.

#![allow(unsafe_code)] // AppKit's pasteboard through objc2, whose generated methods are all `unsafe`; see the essay.

use std::path::{Path, PathBuf};
use std::process::Child;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString, NSURL};

use crate::clipboard::ClipError;
use crate::platform::pasteboard::{self, Read};

/// What [`ClipError::Missing`] says here. The pasteboard is always there,
/// so nothing here refuses that way.
pub fn missing(_tool: &str) -> String {
    "The pasteboard is not available".to_string()
}

/// Put `bytes` on the pasteboard, offered as `mime` (or as text when it is
/// `None`). The copy is made before this returns, so it is `Ok(None)`: no
/// process for the window to own.
pub fn copy(mime: Option<&str>, bytes: &[u8]) -> Result<Option<Child>, ClipError> {
    copy_to(&general(), mime, bytes).map(|()| None)
}

/// Stop and collect a child. [`copy`] never hands one out here, so nothing
/// calls this with one of ours; a child that is handed in all the same is
/// stopped and collected rather than left behind.
pub fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The mime types the pasteboard is offering, in its order, that this
/// program can paste.
pub fn offered_types() -> Result<Vec<String>, ClipError> {
    Ok(offered_by(&general()))
}

/// The pasteboard's bytes, as `mime`.
pub fn paste(mime: &str) -> Result<Vec<u8>, ClipError> {
    paste_from(&general(), mime)
}

/// How many times the pasteboard has changed hands: a number that moves
/// whenever anything, this program included, puts something on it.
pub fn change_count() -> isize {
    // SAFETY: a read of an integer property of the shared pasteboard.
    unsafe { general().changeCount() }
}

/// The pasteboard everybody shares.
fn general() -> Retained<NSPasteboard> {
    // SAFETY: a class method with no arguments that always answers.
    unsafe { NSPasteboard::generalPasteboard() }
}

fn copy_to(board: &NSPasteboard, mime: Option<&str>, bytes: &[u8]) -> Result<(), ClipError> {
    autoreleasepool(|_| {
        let wrote = match mime {
            Some(mime) if mime.eq_ignore_ascii_case(pasteboard::URI_LIST) => {
                let paths = crate::clipboard::parse_uri_list(&String::from_utf8_lossy(bytes));
                write_files(board, &paths)?
            }
            None => write_text(board, bytes),
            Some(mime) if mime.starts_with("text/") => write_text(board, bytes),
            Some(mime) => {
                let uti = pasteboard::image_uti(mime, bytes).ok_or_else(|| {
                    ClipError::Failed(format!("{mime} cannot go on the pasteboard"))
                })?;
                let data = NSData::with_bytes(bytes);
                // SAFETY: the pasteboard is cleared before it is written to,
                // which is what makes this program its owner, and both
                // arguments are live objects.
                unsafe {
                    board.clearContents();
                    board.setData_forType(Some(&data), &NSString::from_str(uti))
                }
            }
        };
        if wrote {
            Ok(())
        } else {
            Err(ClipError::Failed(
                "The pasteboard refused the copy".to_string(),
            ))
        }
    })
}

/// Text, as the pasteboard's string. Bytes that are not UTF-8 are carried
/// as far as they can be: the pasteboard's string is Unicode, and a
/// replacement character is a visible sign of what could not be.
fn write_text(board: &NSPasteboard, bytes: &[u8]) -> bool {
    let text = NSString::from_str(&String::from_utf8_lossy(bytes));
    // SAFETY: cleared first, as above; both arguments are live objects.
    unsafe {
        board.clearContents();
        board.setString_forType(&text, &NSString::from_str(pasteboard::TEXT))
    }
}

/// Files, one `NSURL` each.
fn write_files(board: &NSPasteboard, paths: &[PathBuf]) -> Result<bool, ClipError> {
    if paths.is_empty() {
        return Err(ClipError::Failed("There were no files to copy".to_string()));
    }
    let mut urls: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = Vec::new();
    for path in paths {
        urls.push(ProtocolObject::from_retained(file_url(path)?));
    }
    let urls = NSArray::from_vec(urls);
    // SAFETY: cleared first, as above; the array holds live URLs, each of
    // which conforms to `NSPasteboardWriting`.
    Ok(unsafe {
        board.clearContents();
        board.writeObjects(&urls)
    })
}

/// A file URL for `path`. Every name on an APFS or HFS+ volume is Unicode;
/// a name that is not cannot be put in an `NSURL` exactly, and a wrong
/// path on the pasteboard is worse than a refused copy.
fn file_url(path: &Path) -> Result<Retained<NSURL>, ClipError> {
    let text = path.to_str().ok_or_else(|| {
        ClipError::Failed(format!(
            "{}: the pasteboard can only carry Unicode names",
            path.display()
        ))
    })?;
    // SAFETY: a class method taking a live string.
    Ok(unsafe { NSURL::fileURLWithPath(&NSString::from_str(text)) })
}

fn offered_by(board: &NSPasteboard) -> Vec<String> {
    autoreleasepool(|_| {
        // SAFETY: a read of the pasteboard's type list.
        let Some(types) = (unsafe { board.types() }) else {
            return Vec::new();
        };
        let names: Vec<String> = types.iter().map(|uti| uti.to_string()).collect();
        pasteboard::offered(names.iter().map(String::as_str))
    })
}

fn paste_from(board: &NSPasteboard, mime: &str) -> Result<Vec<u8>, ClipError> {
    let nothing = || ClipError::Failed("nothing on the clipboard".to_string());
    autoreleasepool(|_| match pasteboard::read_for(mime).ok_or_else(nothing)? {
        Read::Files => {
            let paths = file_paths(board);
            if paths.is_empty() {
                return Err(nothing());
            }
            Ok(crate::clipboard::uri_list(&paths).into_bytes())
        }
        Read::Text => {
            // SAFETY: a read of the pasteboard's string, by a live type name.
            let text = unsafe { board.stringForType(&NSString::from_str(pasteboard::TEXT)) };
            text.map(|text| text.to_string().into_bytes())
                .ok_or_else(nothing)
        }
        Read::Data(uti) => {
            // SAFETY: a read of the pasteboard's data, by a live type name.
            let data = unsafe { board.dataForType(&NSString::from_str(uti)) };
            data.map(|data| data.bytes().to_vec()).ok_or_else(nothing)
        }
    })
}

/// Every item's file URL, as a path. An item with no file URL, or one that
/// no longer resolves to a path, is skipped.
fn file_paths(board: &NSPasteboard) -> Vec<PathBuf> {
    // SAFETY: a read of the pasteboard's items.
    let Some(items) = (unsafe { board.pasteboardItems() }) else {
        return Vec::new();
    };
    let file_url = NSString::from_str(pasteboard::FILE_URL);
    items
        .iter()
        .filter_map(|item| {
            // SAFETY: reads of a live item's string, a URL made from it, and
            // that URL's path; each answers `None` rather than failing.
            unsafe {
                let text = item.stringForType(&file_url)?;
                let url = NSURL::URLWithString(&text)?;
                let path = url.filePathURL()?.path()?;
                Some(PathBuf::from(path.to_string()))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pasteboard of the test's own, so the machine's clipboard (and any
    /// other test's) is left as it was. It is served by the same pasteboard
    /// server as the general one.
    fn private() -> Retained<NSPasteboard> {
        // SAFETY: a class method with no arguments that always answers.
        unsafe { NSPasteboard::pasteboardWithUniqueName() }
    }

    #[test]
    fn text_round_trips_through_the_pasteboard() {
        let board = private();
        copy_to(&board, None, "naïve café — ✓".as_bytes()).expect("copied");
        assert_eq!(offered_by(&board), vec![pasteboard::TEXT_MIME.to_string()]);
        let back = paste_from(&board, pasteboard::TEXT_MIME).expect("pasted");
        assert_eq!(String::from_utf8(back).expect("utf-8"), "naïve café — ✓");
    }

    #[test]
    fn a_list_of_files_round_trips_through_the_pasteboard() {
        let dir = std::env::temp_dir().join(format!("df-pasteboard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let files = [dir.join("a b.txt"), dir.join("ünïcode #1.png")];
        for file in &files {
            std::fs::write(file, b"x").expect("fixture");
        }
        let board = private();
        let list = crate::clipboard::uri_list(&files);
        copy_to(&board, Some(pasteboard::URI_LIST), list.as_bytes()).expect("copied");
        assert!(
            offered_by(&board).contains(&pasteboard::URI_LIST.to_string()),
            "{:?}",
            offered_by(&board)
        );
        let back = paste_from(&board, pasteboard::URI_LIST).expect("pasted");
        let back = crate::clipboard::parse_uri_list(&String::from_utf8(back).expect("utf-8"));
        // `/var/folders` is a link to `/private/var/folders`; either spelling
        // is the same file.
        let canonical = |paths: &[PathBuf]| -> Vec<PathBuf> {
            paths
                .iter()
                .map(|p| p.canonicalize().expect("exists"))
                .collect()
        };
        assert_eq!(canonical(&back), canonical(&files));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_image_round_trips_under_its_own_type() {
        let board = private();
        let png = b"\x89PNG\r\n\x1a\nnot really the rest of a png";
        copy_to(&board, Some("image/png"), png).expect("copied");
        assert_eq!(offered_by(&board), vec!["image/png".to_string()]);
        assert_eq!(paste_from(&board, "image/png").expect("pasted"), png);
        assert!(
            copy_to(&board, Some("image/avif"), b"not a png").is_err(),
            "an image with no pasteboard name is refused, not mislabelled"
        );
    }

    #[test]
    fn a_paste_of_what_is_not_there_fails() {
        let board = private();
        copy_to(&board, None, b"text only").expect("copied");
        assert!(paste_from(&board, pasteboard::URI_LIST).is_err());
        assert!(paste_from(&board, "image/png").is_err());
        assert!(paste_from(&board, "application/pdf").is_err());
    }
}
