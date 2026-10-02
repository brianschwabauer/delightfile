//! Printing a PDF through the desktop's own print dialog.
//!
//! On Linux this is the XDG desktop portal's `org.freedesktop.portal.Print`
//! (version 4 here, served by `xdg-desktop-portal-gtk`): `PreparePrint` puts
//! up the GTK print dialog — printer, copies, pages, duplex, paper — and
//! answers a token; `Print` takes a PDF as a file descriptor and the token
//! and prints without a second dialog. The body is written in
//! `print/linux-portal`; this is the seam both halves of the feature were
//! built against.

use std::path::Path;

/// Whether this platform can print at all: whether `builtin:print` is
/// offered and `Command::Print` is live.
pub const SUPPORTED: bool = true;

/// A print dialog that was confirmed: what the person chose, as the token
/// the portal hands back, to be redeemed by [`Session::print`] once the PDF
/// exists.
#[derive(Debug)]
pub struct Session {
    token: u32,
}

/// Put up the system's print dialog titled `title` and wait for the person
/// to answer it. `Ok(None)` is the dialog cancelled. Blocks for as long as a
/// person takes: never call this on the UI thread.
pub fn prepare(title: &str) -> Result<Option<Session>, String> {
    let _ = title;
    Err("printing is not written yet".to_string())
}

impl Session {
    /// Print `pdf` with the settings the dialog was answered with. Blocks
    /// until the portal has taken the job (not until the paper is out).
    pub fn print(self, pdf: &Path) -> Result<(), String> {
        let _ = (self.token, pdf);
        Err("printing is not written yet".to_string())
    }
}
