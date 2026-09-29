//! The desktop device on Linux: the Wayland data device
//! ([`crate::wayland::DataDevice`]), adopted from winit's own connection.
//! Its API is the platform's: `ready`, `set_selection`, `receive`, `drag` and
//! `poll`, answering in [`crate::platform::desktop::Event`]s.

use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::app::Waker;

pub use crate::wayland::DataDevice as Desktop;

/// Adopt winit's Wayland connection for [`crate::wayland`].
///
/// `None` on X11, on a compositor without the protocol, or when the handles
/// cannot be had — all of which are "this desktop does not do that", not
/// errors.
pub fn start(event_loop: &ActiveEventLoop, window: &Window, waker: Waker) -> Option<Desktop> {
    use winit::raw_window_handle::{
        HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
    };
    let RawDisplayHandle::Wayland(display) = event_loop.display_handle().ok()?.as_raw() else {
        return None;
    };
    let RawWindowHandle::Wayland(surface) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: both handles describe objects winit owns and keeps alive for
    // the lifetime of the window, and the device is dropped in `finish`
    // before the window is. The workspace warns on `unsafe_code`; this is
    // the one call site in df-app outside `crate::wayland`, and it is here
    // rather than inside that module because this is where the two handles
    // — and the promise about their lifetime — actually come from.
    #[allow(unsafe_code)]
    unsafe {
        crate::wayland::DataDevice::start(display.display, surface.surface, waker)
    }
}
