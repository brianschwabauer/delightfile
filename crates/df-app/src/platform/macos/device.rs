//! The desktop device on macOS: none yet. Dragging out through an
//! `NSDraggingSession` and the pasteboard behind `set_selection` and
//! `receive` are Phase 2's (`plans/other-platforms/02-macos.md` M2.12,
//! M2.13); until then [`start`] answers `None`, which the window already
//! takes as "drag out is off and the clipboard goes the other way".

use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::app::Waker;
use crate::platform::desktop::Event;
use crate::platform::icon::Rgba;

/// The device. No value of it exists on this platform yet — [`start`] never
/// makes one — so none of these is ever called.
pub enum Desktop {}

impl Desktop {
    pub fn ready(&self) -> bool {
        match *self {}
    }

    #[must_use]
    pub fn set_selection(&self, _mimes: Vec<String>, _bytes: Vec<u8>) -> bool {
        match *self {}
    }

    #[must_use]
    pub fn receive(&self, _seq: u64, _mime: String) -> bool {
        match *self {}
    }

    #[must_use]
    pub fn drag(
        &self,
        _offers: Vec<(String, Vec<u8>)>,
        _count: usize,
        _card: Rgba,
        _ink: Rgba,
        _scale: i32,
    ) -> bool {
        match *self {}
    }

    pub fn poll(&self) -> Vec<Event> {
        match *self {}
    }
}

/// No device on this platform yet.
pub fn start(_event_loop: &ActiveEventLoop, _window: &Window, _waker: Waker) -> Option<Desktop> {
    None
}

/// Where the pointer is over `window`, in logical points, for a drop winit
/// reports without a position (`App::winit_drop`). Not asked yet (M2.11):
/// `None`, and the caller falls back to the last position egui saw.
pub fn pointer_position(_window: &Window) -> Option<(f32, f32)> {
    None
}
