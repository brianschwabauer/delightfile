//! The desktop device on Windows: the clipboard, answered at once
//! (`plans/other-platforms/04-windows.md` W4.16, W4.17).
//!
//! The window's clipboard is written against the Wayland data device's
//! shape — a copy is offered and answered later, a paste asked for and
//! answered later, a mirror of what the clipboard holds kept up to date
//! (`platform::desktop`'s essay). The Windows clipboard is synchronous, as
//! macOS's pasteboard is, so this device does each thing when it is asked,
//! through the functions the window's fallback calls ([`super::clipboard`]),
//! and queues the answer for the next [`Desktop::poll`]: the window sees the
//! events it sees on Linux, one frame later.
//!
//! The mirror is the clipboard's sequence number, which moves whenever the
//! clipboard changes hands; [`Desktop::poll`] reads it (a cheap call, but a
//! frame can come every few milliseconds, so at most every
//! [`MIRROR_EVERY`]) and, when it has moved, answers [`Event::Selection`]
//! with the types now on offer.
//!
//! Dragging out needs OLE's `IDataObject` and `IDropSource` COM objects,
//! deferred with their design recorded (W4.27), so [`Desktop::drag`] says no
//! and the window springs the ghost home. A drop *in* arrives as winit's
//! file events; [`pointer_position`] says where the pointer is when it does.
#![allow(unsafe_code)] // GetCursorPos and ScreenToClient, each writing one POINT this file owns

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
use winit::event_loop::ActiveEventLoop;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::app::Waker;
use crate::platform::desktop::{Event, PasteFailure};
use crate::platform::icon::Rgba;

/// Whether the window hands a drag off from its `CursorMoved` arm rather
/// than from the frame: no, and there is no drag to hand off (W4.27).
pub const HANDS_OFF_ON_CURSOR_MOVED: bool = false;

/// How often the clipboard's sequence number is read.
const MIRROR_EVERY: Duration = Duration::from_millis(200);

/// The device: the answers waiting for the next poll, and the mirror's
/// bookkeeping.
pub struct Desktop {
    waker: Waker,
    answers: RefCell<Vec<Event>>,
    /// The sequence number the mirror was last taken at; `None` before the
    /// first poll, so the first poll always mirrors.
    seen: Cell<Option<u32>>,
    looked: Cell<Option<Instant>>,
}

impl Desktop {
    /// Always: there is no thread here to have stopped.
    pub fn ready(&self) -> bool {
        true
    }

    /// Copy `bytes` onto the clipboard now, and answer [`Event::Copied`] on
    /// the next poll. `mimes` is the offer `crate::clipboard::offer_mimes`
    /// makes: text is its `text/plain` names, anything else its one type.
    #[must_use]
    pub fn set_selection(&self, mimes: Vec<String>, bytes: Vec<u8>) -> bool {
        let ok = match super::clipboard::copy(copied_as(&mimes), &bytes) {
            Ok(_) => true,
            Err(error) => {
                log::info!("clipboard: the copy was refused: {error}");
                false
            }
        };
        self.answer(Event::Copied { ok });
        true
    }

    /// Read the clipboard as `mime` now, and answer [`Event::Pasted`] with
    /// `seq` on the next poll.
    #[must_use]
    pub fn receive(&self, seq: u64, mime: String) -> bool {
        let bytes = super::clipboard::paste(&mime).map_err(|error| {
            log::info!("clipboard: nothing came off the clipboard as {mime}: {error}");
            PasteFailure::Gone
        });
        self.answer(Event::Pasted { seq, bytes });
        true
    }

    /// No drag leaves the window here yet (W4.27): `false`, and the window
    /// springs the ghost home.
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
    /// clipboard if it has changed hands.
    pub fn poll(&self) -> Vec<Event> {
        let mut events = Vec::new();
        let now = Instant::now();
        let due = self
            .looked
            .get()
            .is_none_or(|looked| now.saturating_duration_since(looked) >= MIRROR_EVERY);
        if due {
            self.looked.set(Some(now));
            let sequence = super::clipboard::sequence();
            if self.seen.get() != Some(sequence) {
                self.seen.set(Some(sequence));
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

/// The mime a copy offered as `mimes` goes onto the clipboard as: `None` for
/// text, whichever `text/plain` spelling it was offered under, and the one
/// type otherwise.
fn copied_as(mimes: &[String]) -> Option<&str> {
    mimes
        .first()
        .map(String::as_str)
        .filter(|mime| !mime.starts_with("text/plain"))
}

/// The device, with the clipboard's copies owned by `window`.
pub fn start(_event_loop: &ActiveEventLoop, window: &Window, waker: Waker) -> Option<Desktop> {
    if let Some(hwnd) = hwnd_of(window) {
        super::clipboard::own_with(hwnd);
    }
    Some(Desktop {
        waker,
        answers: RefCell::new(Vec::new()),
        seen: Cell::new(None),
        looked: Cell::new(None),
    })
}

/// Where the pointer is over `window`, in logical points, for a drop winit
/// reports without a position (`App::winit_drop`): the system's cursor, in
/// the window's client area, divided by its scale.
pub fn pointer_position(window: &Window) -> Option<(f32, f32)> {
    let hwnd = hwnd_of(window)?;
    let mut at = POINT { x: 0, y: 0 };
    // SAFETY: both calls write the one POINT, live for their length; the
    // window handle is winit's, alive as long as `window`.
    let known = unsafe { GetCursorPos(&mut at) != 0 && ScreenToClient(hwnd, &mut at) != 0 };
    if !known {
        return None;
    }
    let scale = window.scale_factor();
    Some((
        (f64::from(at.x) / scale) as f32,
        (f64::from(at.y) / scale) as f32,
    ))
}

/// winit's window handle for `window`.
fn hwnd_of(window: &Window) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}
