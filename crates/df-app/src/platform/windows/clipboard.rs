//! The clipboard on Windows (`plans/other-platforms/04-windows.md` W4.16):
//! the system's own, opened, read or written, and closed.
//!
//! It is synchronous, like macOS's pasteboard: `SetClipboardData` hands the
//! system a block of memory and the copy is made, so [`copy`] answers
//! `Ok(None)` — a copy already made, with no process for the window to own —
//! and a paste is a read that returns the bytes.
//!
//! **The formats.** Text is `CF_UNICODETEXT`. A list of files is `CF_HDROP`,
//! what Explorer writes and reads, with the registered `Preferred DropEffect`
//! saying copy, so a paste in Explorer copies rather than moves. A picture
//! is the registered `PNG` format, which Paint, the Snipping Tool, browsers
//! and Office read and write; a picture that is not a PNG is made into one on
//! the way (the `image` crate already decodes every format the window copies
//! by value). A picture another program offered only as a bitmap (`CF_DIB`,
//! `CF_DIBV5` — an old program, a bare Print Screen) is made into a PNG on
//! the way in. What each of those is called as a mime, both ways, is
//! [`offered_types`].
//!
//! **Opening it.** Only one program has the clipboard open at a time, and
//! another may be holding it for a moment, so [`Open`] tries ten times ten
//! milliseconds before it gives up, and closes it again however the call
//! ends. It is opened for the window ([`own_with`]), which is the owner a
//! copy leaves behind; before there is a window — a test — for no window,
//! which the system takes as well.
//!
//! **Memory.** A block is `GlobalAlloc`'d, filled under `GlobalLock`, and
//! given to `SetClipboardData`, after which it is the system's; if the
//! system refuses it, it is still ours and is freed ([`Global`]). A block the
//! clipboard hands out on a paste is the system's and is only read, under
//! `GlobalLock`, for no longer than its `GlobalSize`.

#![allow(unsafe_code)] // the clipboard and global memory through windows-sys; every block's owner is said where it changes hands

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::System::Ole::{
    CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY,
};
use windows_sys::Win32::UI::Shell::DragQueryFileW;

use crate::clipboard::ClipError;
use crate::platform::clipformats;

/// The mimes this clipboard speaks.
const URI_LIST: &str = "text/uri-list";
const TEXT: &str = "text/plain;charset=utf-8";
const PNG_MIME: &str = "image/png";

/// The window a copy is owned by, once there is one ([`own_with`]).
static OWNER: AtomicIsize = AtomicIsize::new(0);

/// Own the clipboard's copies with `window`: the desktop device's start.
pub fn own_with(window: HWND) {
    OWNER.store(window, Ordering::Relaxed);
}

/// What [`ClipError::Missing`] says here. The clipboard is always there, so
/// nothing here refuses that way.
pub fn missing(_tool: &str) -> String {
    "The clipboard is not available".to_string()
}

/// Put `bytes` on the clipboard, offered as `mime` (or as text when it is
/// `None`). The copy is made before this returns, so it is `Ok(None)`.
pub fn copy(mime: Option<&str>, bytes: &[u8]) -> Result<Option<Child>, ClipError> {
    let blocks = blocks_for(mime, bytes)?;
    let _open = Open::new()?;
    // SAFETY: the clipboard is open (`_open`); emptying it is what makes the
    // opener its owner, and must come before anything is set.
    if unsafe { EmptyClipboard() } == 0 {
        return Err(ClipError::Failed(
            "The clipboard would not empty".to_string(),
        ));
    }
    for (format, block) in blocks {
        Global::with(&block)?.give(format)?;
    }
    Ok(None)
}

/// Stop and collect a child. [`copy`] never hands one out here, so nothing
/// calls this with one of ours; a child that is handed in all the same is
/// stopped and collected rather than left behind.
pub fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The mimes the clipboard is offering, in the owner's order, that this
/// program can paste: `CF_HDROP` is `text/uri-list`, `CF_UNICODETEXT` (which
/// Windows makes from any text) is text, and `PNG` or a bitmap is
/// `image/png`.
pub fn offered_types() -> Result<Vec<String>, ClipError> {
    let png = registered("PNG");
    let _open = Open::new()?;
    let mut mimes: Vec<String> = Vec::new();
    let mut format = 0;
    loop {
        // SAFETY: the clipboard is open; the call walks its format list.
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            break;
        }
        let mime = match format {
            f if f == u32::from(CF_HDROP) => URI_LIST,
            f if f == u32::from(CF_UNICODETEXT) => TEXT,
            f if f == png || f == u32::from(CF_DIB) || f == u32::from(CF_DIBV5) => PNG_MIME,
            _ => continue,
        };
        if !mimes.iter().any(|m| m == mime) {
            mimes.push(mime.to_string());
        }
    }
    Ok(mimes)
}

/// The clipboard's bytes, as `mime`.
pub fn paste(mime: &str) -> Result<Vec<u8>, ClipError> {
    let nothing = || ClipError::Failed("nothing on the clipboard".to_string());
    let base = mime.split(';').next().unwrap_or(mime).trim();
    let png = registered("PNG");
    let _open = Open::new()?;
    if base.eq_ignore_ascii_case(URI_LIST) {
        let paths = dropped_paths().ok_or_else(nothing)?;
        if paths.is_empty() {
            return Err(nothing());
        }
        return Ok(crate::clipboard::uri_list(&paths).into_bytes());
    }
    if base.starts_with("text/") {
        let block = read(u32::from(CF_UNICODETEXT)).ok_or_else(nothing)?;
        return Ok(clipformats::text_of_unicode(&block).into_bytes());
    }
    if base.eq_ignore_ascii_case(PNG_MIME) {
        if png != 0 {
            if let Some(block) = read(png) {
                return Ok(block);
            }
        }
        for bitmap in [CF_DIBV5, CF_DIB] {
            if let Some(png) = read(u32::from(bitmap)).and_then(|dib| clipformats::png_of_dib(&dib))
            {
                return Ok(png);
            }
        }
    }
    Err(nothing())
}

/// The clipboard's change count: a number that moves whenever anything,
/// this program included, puts something on it. The device's mirror.
pub fn sequence() -> u32 {
    // SAFETY: no arguments; a read of a counter.
    unsafe { GetClipboardSequenceNumber() }
}

/// The formats and bytes one copy puts on the clipboard.
fn blocks_for(mime: Option<&str>, bytes: &[u8]) -> Result<Vec<(u32, Vec<u8>)>, ClipError> {
    let base = mime.map(|m| m.split(';').next().unwrap_or(m).trim());
    match base {
        None => Ok(vec![(
            u32::from(CF_UNICODETEXT),
            clipformats::unicode_text(bytes),
        )]),
        Some(m) if m.eq_ignore_ascii_case(URI_LIST) => {
            let paths = crate::clipboard::parse_uri_list(&String::from_utf8_lossy(bytes));
            if paths.is_empty() {
                return Err(ClipError::Failed("There were no files to copy".to_string()));
            }
            let wide: Vec<Vec<u16>> = paths
                .iter()
                .map(|path| path.as_os_str().encode_wide().collect())
                .collect();
            let effect = registered("Preferred DropEffect");
            let mut blocks = vec![(u32::from(CF_HDROP), clipformats::dropfiles(&wide))];
            if effect != 0 {
                blocks.push((effect, DROPEFFECT_COPY.to_le_bytes().to_vec()));
            }
            Ok(blocks)
        }
        Some(m) if m.starts_with("text/") => Ok(vec![(
            u32::from(CF_UNICODETEXT),
            clipformats::unicode_text(bytes),
        )]),
        Some(m) if m.starts_with("image/") => {
            let png = registered("PNG");
            if png == 0 {
                return Err(ClipError::Failed(
                    "The clipboard has no PNG format".to_string(),
                ));
            }
            let bytes = if m.eq_ignore_ascii_case(PNG_MIME) {
                bytes.to_vec()
            } else {
                as_png(bytes)
                    .ok_or_else(|| ClipError::Failed(format!("{m} cannot go on the clipboard")))?
            };
            Ok(vec![(png, bytes)])
        }
        Some(m) => Err(ClipError::Failed(format!("{m} cannot go on the clipboard"))),
    }
}

/// A picture in any format the window copies by value, as a PNG.
fn as_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let image = image::load_from_memory(bytes).ok()?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

/// A registered format's number, registering it if nobody has yet; `0` if
/// the system will not say.
fn registered(name: &str) -> u32 {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: a NUL-terminated string that outlives the call.
    unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
}

/// The paths of the clipboard's `CF_HDROP`, if it has one. The clipboard
/// must be open.
fn dropped_paths() -> Option<Vec<PathBuf>> {
    // SAFETY: the clipboard is open; the handle is the system's, read only
    // through `DragQueryFileW`, which checks it, and never freed here.
    unsafe {
        if IsClipboardFormatAvailable(u32::from(CF_HDROP)) == 0 {
            return None;
        }
        let drop = GetClipboardData(u32::from(CF_HDROP));
        if drop == 0 {
            return None;
        }
        let count = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let len = DragQueryFileW(drop, index, std::ptr::null_mut(), 0);
            let mut buffer = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(drop, index, buffer.as_mut_ptr(), buffer.len() as u32);
            buffer.truncate(got as usize);
            paths.push(PathBuf::from(OsString::from_wide(&buffer)));
        }
        Some(paths)
    }
}

/// A copy of the clipboard's block in `format`, if it has one. The clipboard
/// must be open.
fn read(format: u32) -> Option<Vec<u8>> {
    // SAFETY: the clipboard is open; the block is the system's, locked for
    // the copy and read for no more than its own size, then unlocked.
    unsafe {
        if IsClipboardFormatAvailable(format) == 0 {
            return None;
        }
        let handle = GetClipboardData(format);
        if handle == 0 {
            return None;
        }
        let block = handle as HGLOBAL;
        let size = GlobalSize(block);
        let data = GlobalLock(block);
        if data.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(data.cast::<u8>(), size).to_vec();
        GlobalUnlock(block);
        Some(bytes)
    }
}

/// The clipboard, open, for as long as this lives.
struct Open;

impl Open {
    /// Open the clipboard for the window, trying for a tenth of a second
    /// while somebody else has it.
    fn new() -> Result<Open, ClipError> {
        let owner = OWNER.load(Ordering::Relaxed);
        for attempt in 0..10 {
            // SAFETY: `owner` is the window's handle or null, which the
            // call takes as "no window".
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Open);
            }
            if attempt < 9 {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Err(ClipError::Failed(
            "Another program is holding the clipboard".to_string(),
        ))
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: this program opened the clipboard and closes it once.
        unsafe { CloseClipboard() };
    }
}

/// A block of global memory this program still owns.
struct Global(HGLOBAL);

impl Global {
    /// A block holding `bytes` (at least one byte, which the call wants).
    fn with(bytes: &[u8]) -> Result<Global, ClipError> {
        let failed = || ClipError::Failed("Out of memory for the clipboard".to_string());
        // SAFETY: a fresh allocation, filled under its lock with no more than
        // it holds, and unlocked before it is handed on.
        unsafe {
            let block = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
            if block.is_null() {
                return Err(failed());
            }
            let global = Global(block);
            let data = GlobalLock(block);
            if data.is_null() {
                return Err(failed());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.cast::<u8>(), bytes.len());
            GlobalUnlock(block);
            Ok(global)
        }
    }

    /// Hand the block to the clipboard as `format`: the system's from here
    /// on, unless it refuses, in which case it is freed with `self`.
    fn give(self, format: u32) -> Result<(), ClipError> {
        // SAFETY: the clipboard is open and emptied by the caller; the block
        // is ours, unlocked, and not used again once the system has it.
        let taken = unsafe { SetClipboardData(format, self.0 as HANDLE) };
        if taken == 0 {
            return Err(ClipError::Failed(
                "The clipboard refused the copy".to_string(),
            ));
        }
        std::mem::forget(self);
        Ok(())
    }
}

impl Drop for Global {
    fn drop(&mut self) {
        // SAFETY: the block is still ours (`give` forgets it once it is not).
        unsafe { GlobalFree(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// The clipboard is the machine's, so the tests that use it take turns.
    static TURN: Mutex<()> = Mutex::new(());

    /// Text and a list of files go through the real clipboard and come back
    /// as they went, and a list of files is offered as one.
    #[test]
    fn text_and_files_round_trip_through_the_clipboard() {
        let _turn = TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        copy(None, "naïve café — ✓".as_bytes()).expect("copied");
        assert!(offered_types()
            .expect("offered")
            .contains(&TEXT.to_string()));
        let back = paste(TEXT).expect("pasted");
        assert_eq!(String::from_utf8(back).expect("utf-8"), "naïve café — ✓");

        let dir = std::env::temp_dir().join(format!("df-clipboard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let files = vec![dir.join("a b.txt"), dir.join("ünïcode #1.png")];
        for file in &files {
            std::fs::write(file, b"x").expect("fixture");
        }
        let list = crate::clipboard::uri_list(&files);
        copy(Some(URI_LIST), list.as_bytes()).expect("copied");
        let offered = offered_types().expect("offered");
        assert_eq!(
            offered.first().map(String::as_str),
            Some(URI_LIST),
            "{offered:?}"
        );
        let back = paste(URI_LIST).expect("pasted");
        let back = crate::clipboard::parse_uri_list(&String::from_utf8(back).expect("utf-8"));
        assert_eq!(back, files);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_png_round_trips_under_its_own_format() {
        let _turn = TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("encoded");
        let png = png.into_inner();
        copy(Some(PNG_MIME), &png).expect("copied");
        assert!(offered_types()
            .expect("offered")
            .contains(&PNG_MIME.to_string()));
        let back = paste(PNG_MIME).expect("pasted");
        assert_eq!(&back[..png.len()], &png[..], "the block may be rounded up");
        assert!(paste(URI_LIST).is_err(), "no files on the clipboard");
    }
}
