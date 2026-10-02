//! Printing a PDF through the system's print dialog: no body here yet, so
//! `builtin:print` is not in this platform's opener table and
//! `Command::Print` says so. The Linux body is the desktop portal; the
//! native ones are a later round.

use std::path::Path;

/// Whether this platform can print at all: not yet.
pub const SUPPORTED: bool = false;

/// A confirmed print dialog, which never exists here.
#[derive(Debug)]
pub struct Session {
    _never: (),
}

/// The system's print dialog: not available here. `stop` is the caller's
/// way of closing a dialog that is up, which a body here will have to honour.
pub fn prepare(
    title: &str,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Option<Session>, String> {
    let _ = (title, stop);
    Err("Printing is not available on this platform".to_string())
}

impl Session {
    /// Never reached: no [`Session`] is ever made here.
    pub fn print(self, pdf: &Path) -> Result<(), String> {
        let _ = pdf;
        Err("Printing is not available on this platform".to_string())
    }
}
