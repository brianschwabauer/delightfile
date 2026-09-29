//! The desktop's side of dragging and of the clipboard: what the window
//! hears from it, and the handle it is heard through.
//!
//! The handle's shape is the Wayland data device's, because that is the
//! design this program's clipboard and drag state machine in `app.rs` was
//! written against: a copy is *offered* and answered later
//! ([`Event::Copied`]), a paste is *asked for* and answered later
//! ([`Event::Pasted`]), a drag is handed over and ends when the desktop
//! says so ([`Event::DragEnded`]), and every answer comes back through
//! [`Desktop::poll`] once a frame. A platform whose clipboard is synchronous
//! answers on the next `poll` instead of later, and the state machine cannot
//! tell the difference — which is the point of keeping this shape rather
//! than a narrower one.
//!
//! The events are the same on every target, so they are defined here once.
//! The handle and [`start`] are each target's (`device` in its module):
//! the Wayland data device on Linux, the pasteboard on macOS, and on Windows
//! for now no device at all. So is [`pointer_position`], which says where a
//! drop that winit reports without a position is, where the platform can
//! tell.

use std::path::PathBuf;

pub use super::device::{pointer_position, start, Desktop, HANDS_OFF_ON_CURSOR_MOVED};

/// What the pointer's own drag machinery learns from the compositor.
#[derive(Debug, Clone)]
pub enum Event {
    /// A drag — somebody else's, or ours come back — is over our window, at
    /// this surface-local point.
    Enter {
        at: (f32, f32),
        ours: bool,
    },
    Motion {
        at: (f32, f32),
    },
    /// It left without dropping.
    Leave,
    /// It was dropped, and here is what it was carrying. Empty when the offer
    /// held nothing this program can paste.
    Drop {
        paths: Vec<PathBuf>,
        ours: bool,
    },
    /// **Our** outgoing drag is over, whatever became of it.
    DragEnded,
    /// The clipboard changed hands: this is what it now offers, in the order
    /// the owner announced it. Empty when the selection was cleared.
    Selection {
        mimes: Vec<String>,
    },
    /// The answer to a [`Desktop::set_selection`]. `false` means the copy
    /// did *not* happen and the window must say so.
    Copied {
        ok: bool,
    },
    /// The bytes a [`Desktop::receive`] asked for, or why they never came in
    /// full — see [`PasteFailure`].
    ///
    /// `seq` is the number the request carried, echoed back. `p` then `P`
    /// inside one round trip asks twice, and the window is only waiting for the
    /// second one: without the number the first answer to arrive is applied
    /// with the *second* request's meaning, so a `p` that should have asked
    /// before overwriting overwrites.
    Pasted {
        seq: u64,
        bytes: Result<Vec<u8>, PasteFailure>,
    },
}

/// Why a paste came back with nothing usable.
///
/// The distinction is the whole point of this type. A read that ran out of
/// time, or broke on the pipe, used to come back as `Some(what arrived so
/// far)` — indistinguishable from a source that wrote its bytes and closed.
/// So a clipboard that stalled halfway through a 40 MB image wrote 20 MB of
/// it to disk and put up a green toast saying so, and a truncated
/// `text/uri-list` read as "those files are gone". Half a file is not a paste,
/// and the only honest answer to one is red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteFailure {
    /// There was no offer left to ask: the clipboard changed hands between the
    /// keystroke and the read.
    Gone,
    /// The source stopped writing and never closed the pipe. `got` is how much
    /// had arrived — enough to say *where* it stopped, never enough to keep.
    Stalled { got: usize },
    /// The pipe itself failed part way through.
    Broken { got: usize },
}

impl std::fmt::Display for PasteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasteFailure::Gone => f.write_str("The clipboard did not hand anything over"),
            PasteFailure::Stalled { got } => write!(
                f,
                "The clipboard source stopped sending after {}",
                crate::format::human_size(*got as u64)
            ),
            PasteFailure::Broken { got } => write!(
                f,
                "The clipboard broke off after {}",
                crate::format::human_size(*got as u64)
            ),
        }
    }
}
