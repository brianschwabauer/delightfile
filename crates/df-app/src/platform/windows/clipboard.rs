//! The clipboard without a data device, on Windows: none yet.
//! `OpenClipboard` and `CF_HDROP` are Phase 4's
//! (`plans/other-platforms/04-windows.md` W4.16), and it will be synchronous
//! — [`copy`] will answer `Ok(None)`, a copy already made, with no process
//! to own. Until then every call refuses with [`ClipError::Missing`], which
//! the window turns into the red toast a copy or paste that did not happen
//! gets.

use std::process::Child;

use crate::clipboard::ClipError;

/// What [`ClipError::Missing`] says here.
pub fn missing(_tool: &str) -> String {
    "No clipboard transport on this platform".to_string()
}

/// Refused: there is no clipboard to copy to yet.
pub fn copy(_mime: Option<&str>, _bytes: &[u8]) -> Result<Option<Child>, ClipError> {
    Err(ClipError::Missing("clipboard"))
}

/// Stop and collect a child. [`copy`] never hands one out here, so nothing
/// calls this with one of ours; a child that is handed in all the same is
/// stopped and collected rather than left behind.
pub fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Refused: there is no clipboard to ask yet.
pub fn offered_types() -> Result<Vec<String>, ClipError> {
    Err(ClipError::Missing("clipboard"))
}

/// Refused: there is no clipboard to read yet.
pub fn paste(_mime: &str) -> Result<Vec<u8>, ClipError> {
    Err(ClipError::Missing("clipboard"))
}
