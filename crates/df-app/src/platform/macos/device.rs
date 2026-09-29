//! The desktop device on macOS: the pasteboard, answered at once.
//!
//! The window's clipboard is written against the Wayland data device's
//! shape — a copy is offered and answered later, a paste asked for and
//! answered later, a mirror of what the clipboard holds kept up to date
//! (`platform::desktop`'s essay). The pasteboard is synchronous, so this
//! device does each thing when it is asked, through the same functions the
//! window's fallback calls ([`super::clipboard`]), and queues the answer for
//! the next [`Desktop::poll`]: the window sees the events it sees on Linux,
//! one frame later, and cannot tell the difference.
//!
//! Nothing tells a program when the pasteboard changes hands; its
//! `changeCount` moves, and that is all. So [`Desktop::poll`] reads the count
//! (at most every [`MIRROR_EVERY`]) and, when it has moved, answers
//! [`Event::Selection`] with the types now on offer, which is the mirror a
//! paste chooses its type from.
//!
//! It also says where the pointer is when winit reports a drop without a
//! position ([`pointer_position`]), which takes two AppKit calls on the
//! window's own view.
//!
//! Dragging out is not here yet (`plans/other-platforms/02-macos.md` M2.12):
//! [`Desktop::drag`] says no, and the ghost springs home, as it did when
//! there was no device at all.

#![allow(unsafe_code)] // Reads of winit's own NSView and NSWindow through objc2; each call says why it holds.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use objc2_app_kit::NSView;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::app::Waker;
use crate::platform::desktop::{Event, PasteFailure};
use crate::platform::icon::Rgba;

/// How often the pasteboard's change count is read. Each read is a round
/// trip to the pasteboard server, and a frame can come every few
/// milliseconds; a clipboard changed in another program and pasted here
/// within a fifth of a second is still read right, because the paste asks
/// for the bytes themselves.
const MIRROR_EVERY: Duration = Duration::from_millis(200);

/// The device: the pasteboard, and the answers waiting for the next poll.
pub struct Desktop {
    waker: Waker,
    answers: RefCell<Vec<Event>>,
    /// The change count the mirror was last taken at; `None` before the
    /// first poll, so the first poll always mirrors.
    seen: Cell<Option<isize>>,
    looked: Cell<Option<Instant>>,
}

impl Desktop {
    fn new(waker: Waker) -> Desktop {
        Desktop {
            waker,
            answers: RefCell::new(Vec::new()),
            seen: Cell::new(None),
            looked: Cell::new(None),
        }
    }

    /// Always: there is no thread here to have stopped.
    pub fn ready(&self) -> bool {
        true
    }

    /// Copy `bytes` onto the pasteboard now, and answer [`Event::Copied`] on
    /// the next poll. `mimes` is the offer `crate::clipboard::offer_mimes`
    /// makes: text is its `text/plain` names, anything else its one type.
    #[must_use]
    pub fn set_selection(&self, mimes: Vec<String>, bytes: Vec<u8>) -> bool {
        let ok = match super::clipboard::copy(copied_as(&mimes), &bytes) {
            Ok(_) => true,
            Err(error) => {
                log::info!("clipboard: the pasteboard refused the copy: {error}");
                false
            }
        };
        self.answer(Event::Copied { ok });
        true
    }

    /// Read the pasteboard as `mime` now, and answer [`Event::Pasted`] with
    /// `seq` on the next poll.
    #[must_use]
    pub fn receive(&self, seq: u64, mime: String) -> bool {
        let bytes = super::clipboard::paste(&mime).map_err(|error| {
            log::info!("clipboard: nothing came off the pasteboard as {mime}: {error}");
            PasteFailure::Gone
        });
        self.answer(Event::Pasted { seq, bytes });
        true
    }

    /// Not yet (M2.12): the drag is refused, and the window springs the
    /// ghost home.
    #[must_use]
    pub fn drag(
        &self,
        _offers: Vec<(String, Vec<u8>)>,
        _count: usize,
        _card: Rgba,
        _ink: Rgba,
        _scale: i32,
    ) -> bool {
        false
    }

    /// The answers since the last poll, after a fresh mirror of the
    /// pasteboard if it has changed hands.
    pub fn poll(&self) -> Vec<Event> {
        let mut events = Vec::new();
        let now = Instant::now();
        let due = self
            .looked
            .get()
            .is_none_or(|looked| now.saturating_duration_since(looked) >= MIRROR_EVERY);
        if due {
            self.looked.set(Some(now));
            let count = super::clipboard::change_count();
            if self.seen.get() != Some(count) {
                self.seen.set(Some(count));
                events.push(Event::Selection {
                    mimes: super::clipboard::offered_types().unwrap_or_default(),
                });
            }
        }
        events.append(&mut self.answers.borrow_mut());
        events
    }

    fn answer(&self, event: Event) {
        self.answers.borrow_mut().push(event);
        self.waker.wake();
    }
}

/// The mime a copy offered as `mimes` goes onto the pasteboard as: `None`
/// for text, which is the pasteboard's string whichever `text/plain`
/// spelling it was offered under, and the one type otherwise.
fn copied_as(mimes: &[String]) -> Option<&str> {
    mimes
        .first()
        .map(String::as_str)
        .filter(|mime| !mime.starts_with("text/plain"))
}

/// The device, for any window: the pasteboard is the application's, not the
/// window's.
pub fn start(_event_loop: &ActiveEventLoop, _window: &Window, waker: Waker) -> Option<Desktop> {
    Some(Desktop::new(waker))
}

/// Where the pointer is over `window`, in logical points from its top-left
/// corner, for a drop winit reports without a position (`App::winit_drop`).
///
/// AppKit keeps the pointer's place in the window whatever events have been
/// delivered (`mouseLocationOutsideOfEventStream`), in the window's own
/// bottom-up coordinates; winit's view is flipped, so converting the point
/// into the view turns it top-down, in the logical points egui draws in.
/// `None` only for a window that is not an AppKit one.
pub fn pointer_position(window: &Window) -> Option<(f32, f32)> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: the handle is winit's `NSView`, which lives as long as the
    // window this borrow came from, and this runs on the main thread, in the
    // window's own event.
    let view: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let ns_window = view.window()?;
    // SAFETY: a read of a point from a live window.
    let at = unsafe { ns_window.mouseLocationOutsideOfEventStream() };
    let local = view.convertPoint_fromView(at, None);
    Some((local.x as f32, local.y as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_goes_as_the_string_and_anything_else_as_its_type() {
        let offered = |mime: Option<&str>| crate::clipboard::offer_mimes(mime);
        assert_eq!(copied_as(&offered(None)), None);
        assert_eq!(
            copied_as(&offered(Some("text/uri-list"))),
            Some("text/uri-list")
        );
        assert_eq!(copied_as(&offered(Some("image/png"))), Some("image/png"));
        assert_eq!(copied_as(&[]), None);
    }
}
